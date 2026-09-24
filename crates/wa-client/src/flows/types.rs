//! Request and response types of the Flows management API.
//!
//! Source: `flows/guides/flowsapi` (field names, categories, statuses, the
//! validation-error shape, assets) and `flows/guides/lifecycle` (what each
//! status allows). Flow JSON itself is deliberately *not* modelled: it is
//! versioned independently of the Graph API (`flows/guides/versioning`) and
//! Meta validates it server-side, returning [`FlowValidationError`]s, so the
//! crate carries it as an opaque JSON string.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use wa_core::error::ValidationError;
use wa_core::ids::FlowId;

/// Largest Flow JSON Meta accepts. The docs say "10 MB" for both the asset
/// upload and the `flow_json` string without saying which megabyte; the
/// binary one is used so a local check never rejects something Meta would
/// have accepted.
pub const MAX_FLOW_JSON_BYTES: usize = 10 * 1024 * 1024;

/// A string enum Meta may extend: documented values get variants, anything
/// else is kept verbatim in `Unknown(String)`. Carrying the raw value (rather
/// than a unit `#[serde(other)]` variant) means an unknown value survives a
/// read-modify-write round trip — e.g. re-sending a Flow's categories — and
/// still shows up in logs.
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
pub(crate) use wire_enum;

wire_enum! {
    /// What a Flow is for. At least one is required when creating a Flow.
    pub enum FlowCategory {
        /// `SIGN_UP`.
        SignUp = "SIGN_UP",
        /// `SIGN_IN`.
        SignIn = "SIGN_IN",
        /// `APPOINTMENT_BOOKING`.
        AppointmentBooking = "APPOINTMENT_BOOKING",
        /// `LEAD_GENERATION`.
        LeadGeneration = "LEAD_GENERATION",
        /// `CONTACT_US`.
        ContactUs = "CONTACT_US",
        /// `CUSTOMER_SUPPORT`.
        CustomerSupport = "CUSTOMER_SUPPORT",
        /// `SURVEY`.
        Survey = "SURVEY",
        /// `OTHER`.
        Other = "OTHER",
    }
}

wire_enum! {
    /// Where a Flow is in its lifecycle (`flows/guides/lifecycle`).
    pub enum FlowStatus {
        /// Initial state: editable, deletable, sendable only with
        /// `"mode": "draft"` for testing.
        Draft = "DRAFT",
        /// Sendable to customers; can no longer be edited or deleted.
        Published = "PUBLISHED",
        /// Retired by the business: cannot be sent or opened. Terminal.
        Deprecated = "DEPRECATED",
        /// Set by WhatsApp monitoring when the endpoint is unhealthy: cannot
        /// be sent or opened until the endpoint recovers.
        Blocked = "BLOCKED",
        /// Set by WhatsApp monitoring when the endpoint degrades: can be
        /// opened, but at most 10 messages per hour can be sent.
        Throttled = "THROTTLED",
    }
}

wire_enum! {
    /// Kind of asset attached to a Flow.
    pub enum FlowAssetType {
        /// The Flow JSON definition (`flow.json`).
        FlowJson = "FLOW_JSON",
    }
}

/// An error Meta found in a Flow's JSON. Every one of them must be fixed
/// before the Flow can be published.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlowValidationError {
    /// Machine-readable error, e.g. `INVALID_PROPERTY_VALUE`.
    #[serde(default)]
    pub error: String,
    /// Error family, e.g. `FLOW_JSON_ERROR`.
    #[serde(default)]
    pub error_type: String,
    /// Human-readable explanation.
    #[serde(default)]
    pub message: String,
    /// First line of the offending span in the Flow JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    /// Last line of the offending span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    /// First column of the offending span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_start: Option<u32>,
    /// Last column of the offending span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_end: Option<u32>,
    /// Locations in the Flow JSON the error refers to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pointers: Vec<FlowValidationPointer>,
}

/// One location a [`FlowValidationError`] points at.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlowValidationPointer {
    /// First line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    /// Last line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    /// First column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_start: Option<u32>,
    /// Last column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_end: Option<u32>,
    /// Path inside the Flow JSON, e.g. `screens [0]. layout.children [0].type`
    /// (spacing as Meta returns it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Body of `POST /{WABA_ID}/flows`. Build it with [`CreateFlow::new`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreateFlow {
    /// Flow name. Not shown to users.
    pub name: String,
    /// Use cases, at least one.
    pub categories: Vec<FlowCategory>,
    /// Flow to copy. The token must be able to read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clone_flow_id: Option<FlowId>,
    /// URL of the Flow data endpoint. From Flow JSON 3.0 on it is set through
    /// the API rather than in the Flow JSON; Meta says not to send it when
    /// cloning a Flow whose JSON is older than 3.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_uri: Option<String>,
    /// The Flow JSON, as a JSON-encoded string. At most
    /// [`MAX_FLOW_JSON_BYTES`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_json: Option<String>,
    /// Publish right away. Shown in the docs' example request but absent
    /// from its parameter table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish: Option<bool>,
}

impl CreateFlow {
    /// A Flow named `name` for `categories`.
    pub fn new(
        name: impl Into<String>,
        categories: impl IntoIterator<Item = FlowCategory>,
    ) -> Self {
        Self {
            name: name.into(),
            categories: categories.into_iter().collect(),
            clone_flow_id: None,
            endpoint_uri: None,
            flow_json: None,
            publish: None,
        }
    }

    /// Copy an existing Flow.
    #[must_use]
    pub fn clone_flow_id(mut self, flow_id: impl Into<FlowId>) -> Self {
        self.clone_flow_id = Some(flow_id.into());
        self
    }

    /// Set the data endpoint URL.
    #[must_use]
    pub fn endpoint_uri(mut self, uri: impl Into<String>) -> Self {
        self.endpoint_uri = Some(uri.into());
        self
    }

    /// Set the Flow JSON from its text.
    #[must_use]
    pub fn flow_json(mut self, json: impl Into<String>) -> Self {
        self.flow_json = Some(json.into());
        self
    }

    /// Set the Flow JSON from a parsed value (serialized compactly).
    #[must_use]
    pub fn flow_json_value(mut self, json: &serde_json::Value) -> Self {
        self.flow_json = Some(json.to_string());
        self
    }

    /// Publish immediately after creation.
    #[must_use]
    pub fn publish(mut self, publish: bool) -> Self {
        self.publish = Some(publish);
        self
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.name.trim().is_empty() {
            return Err(ValidationError::new("name", "is required"));
        }
        if self.categories.is_empty() {
            return Err(ValidationError::new(
                "categories",
                "at least one category is required",
            ));
        }
        if let Some(json) = &self.flow_json {
            validate_flow_json_len("flow_json", json.len())?;
        }
        Ok(())
    }
}

/// Response of `POST /{WABA_ID}/flows`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CreatedFlow {
    /// Id of the new Flow.
    pub id: FlowId,
    /// Present in the docs' example; `None` when Meta omits it.
    #[serde(default)]
    pub success: Option<bool>,
    /// Problems found in the Flow JSON, if one was sent. A Flow with
    /// validation errors is created (as a draft) but cannot be published.
    #[serde(default)]
    pub validation_errors: Vec<FlowValidationError>,
}

/// Body of `POST /{FLOW_ID}`: metadata to change. Unset fields keep their
/// current value.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UpdateFlow {
    /// New name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New categories; replaces the current list and must not be empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<FlowCategory>>,
    /// New data endpoint URL (Flow JSON 3.0 and later only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_uri: Option<String>,
}

impl UpdateFlow {
    /// Nothing to change yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rename.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Replace the categories.
    #[must_use]
    pub fn categories(mut self, categories: impl IntoIterator<Item = FlowCategory>) -> Self {
        self.categories = Some(categories.into_iter().collect());
        self
    }

    /// Point the Flow at another data endpoint.
    #[must_use]
    pub fn endpoint_uri(mut self, uri: impl Into<String>) -> Self {
        self.endpoint_uri = Some(uri.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.name.is_none() && self.categories.is_none() && self.endpoint_uri.is_none() {
            return Err(ValidationError::new("body", "nothing to update"));
        }
        if self.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(ValidationError::new("name", "must not be empty"));
        }
        if self.categories.as_ref().is_some_and(Vec::is_empty) {
            return Err(ValidationError::new(
                "categories",
                "when provided, at least one category is required",
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_flow_json_len(field: &str, len: usize) -> Result<(), ValidationError> {
    if len == 0 {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    if len > MAX_FLOW_JSON_BYTES {
        return Err(ValidationError::new(
            field,
            format!("is {len} bytes; Meta accepts at most 10 MB"),
        ));
    }
    Ok(())
}

/// A Flow as returned by `GET /{FLOW_ID}` and `GET /{WABA_ID}/flows`.
///
/// `id`, `name`, `status`, `categories` and `validation_errors` are Meta's
/// defaults; the rest are only present when requested through `fields`
/// ([`super::Flow::get`] requests all of them except `preview`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowDetails {
    /// Flow id.
    pub id: FlowId,
    /// Name (not visible to users).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Lifecycle status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FlowStatus>,
    /// Categories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<FlowCategory>,
    /// Errors that block publishing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_errors: Vec<FlowValidationError>,
    /// `version` of the uploaded Flow JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_version: Option<String>,
    /// `data_api_version` of the uploaded Flow JSON (endpoint Flows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_api_version: Option<String>,
    /// Data endpoint URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_uri: Option<String>,
    /// Web preview link, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<FlowPreview>,
    /// The owning WhatsApp Business Account. Meta documents it only as
    /// "object", so it is kept untyped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whatsapp_business_account: Option<serde_json::Value>,
    /// The Meta app that created the Flow. Documented only as "object".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<serde_json::Value>,
}

/// Shareable, login-free web preview of a Flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowPreview {
    /// Preview page; can be embedded in an `<iframe>`. Valid for 30 days or
    /// until invalidated.
    pub preview_url: String,
    /// Expiry as Meta returns it, e.g. `2023-05-21T11:18:09+0000`. Parsed by
    /// [`FlowPreview::expires_at`] rather than at decode time so an
    /// unexpected format never fails the whole response.
    #[serde(rename = "expires_at")]
    pub expires_at_raw: String,
}

/// Graph's ISO 8601 flavour: numeric offset without a colon.
const GRAPH_DATETIME: &[BorrowedFormatItem<'static>] = format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory][offset_minute]"
);

impl FlowPreview {
    /// [`Self::expires_at_raw`] parsed, when it has Graph's usual format.
    pub fn expires_at(&self) -> Option<OffsetDateTime> {
        OffsetDateTime::parse(&self.expires_at_raw, GRAPH_DATETIME).ok()
    }
}

/// An asset attached to a Flow (`GET /{FLOW_ID}/assets`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowAsset {
    /// Asset name, `flow.json` for the Flow JSON.
    pub name: String,
    /// Asset kind.
    pub asset_type: FlowAssetType,
    /// Short-lived CDN link to the asset's content. It is not on the Graph
    /// host, so the client will not attach a token to it; fetch it with a
    /// plain HTTP GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
}

/// Response of a Flow JSON upload (`POST /{FLOW_ID}/assets`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FlowJsonUpload {
    /// Whether the asset was stored. It is stored even when it has
    /// validation errors.
    pub success: bool,
    /// Problems found in the uploaded JSON.
    #[serde(default)]
    pub validation_errors: Vec<FlowValidationError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn categories_round_trip_including_unknown_values() {
        let cats: Vec<FlowCategory> =
            serde_json::from_value(json!(["SIGN_UP", "OTHER", "SHOPPING"])).unwrap();
        assert_eq!(
            cats,
            vec![
                FlowCategory::SignUp,
                FlowCategory::Other,
                FlowCategory::Unknown("SHOPPING".into())
            ]
        );
        assert_eq!(
            serde_json::to_value(&cats).unwrap(),
            json!(["SIGN_UP", "OTHER", "SHOPPING"])
        );
    }

    #[test]
    fn every_documented_status_parses() {
        for (wire, status) in [
            ("DRAFT", FlowStatus::Draft),
            ("PUBLISHED", FlowStatus::Published),
            ("DEPRECATED", FlowStatus::Deprecated),
            ("BLOCKED", FlowStatus::Blocked),
            ("THROTTLED", FlowStatus::Throttled),
        ] {
            assert_eq!(FlowStatus::from(wire), status);
            assert_eq!(status.as_str(), wire);
        }
        assert_eq!(
            FlowStatus::from("ARCHIVED"),
            FlowStatus::Unknown("ARCHIVED".into())
        );
    }

    #[test]
    fn preview_expiry_parses_graph_offsets() {
        let p: FlowPreview = serde_json::from_value(json!({
            "preview_url": "https://business.facebook.com/wa/manage/flows/550.../preview/?token=b9d6....",
            "expires_at": "2023-05-21T11:18:09+0000"
        }))
        .unwrap();
        assert_eq!(p.expires_at().unwrap().unix_timestamp(), 1_684_667_889);
        let odd = FlowPreview {
            preview_url: String::new(),
            expires_at_raw: "tomorrow".into(),
        };
        assert_eq!(odd.expires_at(), None);
    }

    #[test]
    fn create_validation() {
        assert!(
            CreateFlow::new("f", [FlowCategory::Survey])
                .validate()
                .is_ok()
        );
        let e = CreateFlow::new(" ", [FlowCategory::Survey])
            .validate()
            .unwrap_err();
        assert_eq!(e.field, "name");
        let e = CreateFlow::new("f", []).validate().unwrap_err();
        assert_eq!(e.field, "categories");
        let e = CreateFlow::new("f", [FlowCategory::Survey])
            .flow_json("")
            .validate()
            .unwrap_err();
        assert_eq!(e.field, "flow_json");
        let big = "x".repeat(MAX_FLOW_JSON_BYTES + 1);
        let e = CreateFlow::new("f", [FlowCategory::Survey])
            .flow_json(big)
            .validate()
            .unwrap_err();
        assert_eq!(e.field, "flow_json");
        let exact = "x".repeat(MAX_FLOW_JSON_BYTES);
        assert!(
            CreateFlow::new("f", [FlowCategory::Survey])
                .flow_json(exact)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn update_validation() {
        assert_eq!(UpdateFlow::new().validate().unwrap_err().field, "body");
        assert_eq!(
            UpdateFlow::new()
                .categories([])
                .validate()
                .unwrap_err()
                .field,
            "categories"
        );
        assert_eq!(
            UpdateFlow::new().name("").validate().unwrap_err().field,
            "name"
        );
        assert!(
            UpdateFlow::new()
                .endpoint_uri("https://x")
                .validate()
                .is_ok()
        );
    }
}
