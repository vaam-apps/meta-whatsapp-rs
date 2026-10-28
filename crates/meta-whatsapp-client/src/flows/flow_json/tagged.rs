//! An enum of objects told apart by one of their keys (`type` for
//! components, `name` for actions) that keeps any object it does not know.
//!
//! Serde's internally tagged enums drop an unknown tag (`#[serde(other)]`
//! only fits a unit variant), so this generates the impls by hand: the tag
//! picks the variant, the rest of the object decodes into the variant's
//! struct (whose flattened `extra` keeps unknown keys), and anything else
//! becomes `Unknown { raw }` with the object verbatim:
//!
//! - an unknown or missing tag (a component or action this version does not
//!   know);
//! - a known tag whose object does not fit the variant's struct (a property
//!   of another type than the one modelled, a required one missing): Meta
//!   changes a property's shape across Flow JSON versions, and its docs do
//!   not always agree on one (`RadioButtonsGroup`'s `init-value` is a string
//!   in one table and a list in another), so such an object must not fail
//!   the whole document;
//! - a known tag whose struct would not serialize back to the same JSON
//!   value (a `null` property, which an optional field reads as absent):
//!   kept whole rather than silently altered.
//!
//! So parsing never fails here, and serializing what was parsed gives back
//! the same JSON value. The structure around components (the document,
//! screens, layouts) stays strict: see the [module docs](super).

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// `raw` (a JSON object whose `tag` is `wire`) as a `T`, when it decodes as
/// one and `T` serializes back to `raw` (the tag aside, which `T` does not
/// hold); `None` otherwise.
pub(super) fn decode_lossless<T: DeserializeOwned + Serialize>(
    raw: &Value,
    tag: &str,
    wire: &str,
) -> Option<T> {
    let Value::Object(object) = raw else {
        return None;
    };
    let mut body = object.clone();
    body.remove(tag);
    let typed: T = serde_json::from_value(Value::Object(body)).ok()?;
    let Ok(Value::Object(mut back)) = serde_json::to_value(&typed) else {
        return None;
    };
    // `T` never writes the tag: its `extra` cannot hold it (it was removed
    // before decoding) and no modelled field is named after it.
    back.insert(tag.to_owned(), Value::String(wire.to_owned()));
    (back == *object).then_some(typed)
}

macro_rules! tagged_union {
    (
        $(#[$meta:meta])*
        pub enum $name:ident by $tag:literal {
            $( $(#[$vmeta:meta])* $variant:ident($ty:ty) = $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        // Documents built once and sent, not values moved in a hot path:
        // boxing every variant would only make matching and building noisier.
        #[allow(clippy::large_enum_variant)]
        #[derive(Debug, Clone, PartialEq)]
        #[non_exhaustive]
        pub enum $name {
            $( $(#[$vmeta])* $variant($ty), )+
            /// An object this version of `meta-whatsapp-rs` does not model,
            /// kept verbatim so it is sent back unchanged: an unknown or
            /// missing tag, or a known one whose properties do not fit the
            /// typed struct (a property of another type, a required one
            /// missing, a `null`).
            Unknown {
                /// The object as it was parsed.
                raw: ::serde_json::Value,
            },
        }

        impl $name {
            /// The tag as it appears on the wire (`None` for an
            /// [`Self::Unknown`] object without one).
            pub fn tag(&self) -> Option<&str> {
                match self {
                    $( Self::$variant(_) => Some($wire), )+
                    Self::Unknown { raw } => raw.get($tag).and_then(::serde_json::Value::as_str),
                }
            }

            /// The typed variant for `raw` when its tag is known and its
            /// properties fit, [`Self::Unknown`] otherwise.
            fn from_raw(raw: ::serde_json::Value) -> Self {
                let typed = match raw.get($tag).and_then(::serde_json::Value::as_str) {
                    $(
                        Some($wire) => super::tagged::decode_lossless::<$ty>(&raw, $tag, $wire)
                            .map(Self::$variant),
                    )+
                    _ => None,
                };
                typed.unwrap_or(Self::Unknown { raw })
            }
        }

        $(
            impl From<$ty> for $name {
                fn from(value: $ty) -> Self {
                    Self::$variant(value)
                }
            }
        )+

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                #[derive(::serde::Serialize)]
                struct Tagged<'a, T> {
                    #[serde(rename = $tag)]
                    tag: &'static str,
                    #[serde(flatten)]
                    inner: &'a T,
                }
                match self {
                    $( Self::$variant(inner) => Tagged { tag: $wire, inner }.serialize(s), )+
                    Self::Unknown { raw } => raw.serialize(s),
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <::serde_json::Value as ::serde::Deserialize>::deserialize(d)?;
                Ok(Self::from_raw(raw))
            }
        }
    };
}

pub(crate) use tagged_union;
