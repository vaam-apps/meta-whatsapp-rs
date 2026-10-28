//! An enum of objects told apart by one of their keys (`type` for
//! components, `name` for actions) that keeps any object it does not know.
//!
//! Serde's internally tagged enums drop an unknown tag (`#[serde(other)]`
//! only fits a unit variant), so this generates the impls by hand: the tag
//! picks the variant, the rest of the object decodes into the variant's
//! struct (whose flattened `extra` keeps unknown keys), and an unknown or
//! missing tag becomes `Unknown { raw }` with the object verbatim.

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
            /// An object this version of `meta-whatsapp-rs` does not model
            /// (an unknown or missing tag), kept verbatim so it is sent back
            /// unchanged.
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
                use ::serde::de::Error as _;
                let mut raw = <::serde_json::Value as ::serde::Deserialize>::deserialize(d)?;
                let tag = raw.get($tag).and_then(::serde_json::Value::as_str).map(str::to_owned);
                match tag.as_deref() {
                    $(
                        Some($wire) => {
                            if let Some(object) = raw.as_object_mut() {
                                object.remove($tag);
                            }
                            ::serde_json::from_value::<$ty>(raw)
                                .map(Self::$variant)
                                .map_err(|e| D::Error::custom(format!("{} {}: {e}", $tag, $wire)))
                        }
                    )+
                    _ => Ok(Self::Unknown { raw }),
                }
            }
        }
    };
}

pub(crate) use tagged_union;
