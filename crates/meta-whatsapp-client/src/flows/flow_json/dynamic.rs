//! Values that are either static or bound to data (`flows/guides/flowjson`,
//! § Properties).
//!
//! A Flow JSON property is *static* when it holds its value, and *dynamic*
//! when it holds a reference to screen data (`${data.field}`), to what the
//! user typed (`${form.field}`), to another screen's storage
//! (`${screen.SCREEN.data.field}`, Flow JSON 4.0+), or a nested expression
//! wrapped in backticks (Flow JSON 6.0+). On the wire a reference is always
//! a string, whatever the property's type.
//!
//! String properties (labels, texts, captions) hold a reference as their
//! value: they are plain `String`s here, built with [`data_ref`],
//! [`form_ref`], [`screen_data_ref`] or [`screen_form_ref`] when bound.
//! Properties of any other type are a [`Dynamic<T>`].

use serde::de::{DeserializeOwned, Deserializer, Error as _};
use serde::{Deserialize, Serialize, Serializer};

/// `${data.<field>}`: a field of the screen's data (from the endpoint, the
/// message that opened the Flow, or the `navigate` action that led here).
pub fn data_ref(field: &str) -> String {
    format!("${{data.{field}}}")
}

/// `${form.<field>}`: the value of the input named `field` on this screen.
pub fn form_ref(field: &str) -> String {
    format!("${{form.{field}}}")
}

/// `${screen.<screen>.data.<field>}`: another screen's data (global
/// dynamic referencing, Flow JSON 4.0+).
pub fn screen_data_ref(screen: &str, field: &str) -> String {
    format!("${{screen.{screen}.data.{field}}}")
}

/// `${screen.<screen>.form.<field>}`: an input of another screen (global
/// dynamic referencing, Flow JSON 4.0+).
pub fn screen_form_ref(screen: &str, field: &str) -> String {
    format!("${{screen.{screen}.form.{field}}}")
}

/// Whether `s` is a reference or a nested expression rather than a literal:
/// it starts with `${` or is wrapped in backticks.
pub fn is_expression(s: &str) -> bool {
    s.starts_with("${") || (s.len() >= 2 && s.starts_with('`') && s.ends_with('`'))
}

/// A property that holds either its value or a reference to one.
///
/// Parsing keeps what was sent: a value of type `T` is [`Self::Static`]; a
/// string that is a reference (see [`is_expression`]) is
/// [`Self::Expression`], and so is any other string `T` does not accept
/// (Meta reports those itself, and keeping the text means a round trip
/// never changes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dynamic<T> {
    /// The value itself.
    Static(T),
    /// A reference (`${data.x}`) or a backticked expression, verbatim.
    Expression(String),
}

impl<T> Dynamic<T> {
    /// Bound to `${data.<field>}`.
    pub fn data(field: &str) -> Self {
        Self::Expression(data_ref(field))
    }

    /// Bound to `${form.<field>}`.
    pub fn form(field: &str) -> Self {
        Self::Expression(form_ref(field))
    }

    /// Bound to `${screen.<screen>.data.<field>}`.
    pub fn screen_data(screen: &str, field: &str) -> Self {
        Self::Expression(screen_data_ref(screen, field))
    }

    /// Bound to `${screen.<screen>.form.<field>}`.
    pub fn screen_form(screen: &str, field: &str) -> Self {
        Self::Expression(screen_form_ref(screen, field))
    }

    /// Any expression, verbatim (for example a nested one in backticks:
    /// `` `${form.age} > 18` ``).
    pub fn expression(expression: impl Into<String>) -> Self {
        Self::Expression(expression.into())
    }

    /// The static value, if this is one.
    pub fn as_static(&self) -> Option<&T> {
        match self {
            Self::Static(v) => Some(v),
            Self::Expression(_) => None,
        }
    }
}

impl<T> From<T> for Dynamic<T> {
    fn from(value: T) -> Self {
        Self::Static(value)
    }
}

impl<T: Serialize> Serialize for Dynamic<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Static(v) => v.serialize(s),
            Self::Expression(e) => s.serialize_str(e),
        }
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Dynamic<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) if is_expression(&s) => Ok(Self::Expression(s)),
            serde_json::Value::String(s) => {
                match T::deserialize(serde_json::Value::String(s.clone())) {
                    Ok(v) => Ok(Self::Static(v)),
                    Err(_) => Ok(Self::Expression(s)),
                }
            }
            other => T::deserialize(other)
                .map(Self::Static)
                .map_err(D::Error::custom),
        }
    }
}

/// A text property that takes one string or a list of them (`TextBody`,
/// `TextCaption` and `RichText`; each item of a list is a paragraph or a
/// line of Markdown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TextContent {
    /// One string (static, or a reference such as `${data.text}`).
    Text(String),
    /// Several strings.
    Lines(Vec<String>),
}

impl TextContent {
    /// The strings, one for [`Self::Text`].
    pub fn parts(&self) -> Vec<&str> {
        match self {
            Self::Text(t) => vec![t.as_str()],
            Self::Lines(lines) => lines.iter().map(String::as_str).collect(),
        }
    }
}

impl From<&str> for TextContent {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for TextContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<String>> for TextContent {
    fn from(value: Vec<String>) -> Self {
        Self::Lines(value)
    }
}

impl<const N: usize> From<[&str; N]> for TextContent {
    fn from(value: [&str; N]) -> Self {
        Self::Lines(value.iter().map(|s| (*s).to_owned()).collect())
    }
}
