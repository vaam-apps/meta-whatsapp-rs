//! `string_enum!`: Meta enums that are strings on the wire.
//!
//! Meta prints the same enum in different cases on different pages
//! (`"marketing"` in one creation example, `"MARKETING"` in the next, and
//! always upper case in responses), and adds values without notice. So every
//! such enum here parses case-insensitively, keeps unknown values verbatim in
//! `Other(String)` (so a new value never fails a response and survives a
//! round trip), and serializes the canonical spelling from the docs.
//!
//! The catch-all is `Other` rather than `Unknown` on purpose: some Meta enums
//! have a documented value literally called `UNKNOWN` (template quality
//! scores), and one name for the catch-all across every enum beats a special
//! case.

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident => $value:literal,
            )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        $vis enum $name {
            $(
                $(#[$vmeta])*
                $variant,
            )+
            /// A value this version of `wa-client` does not know, kept verbatim.
            Other(String),
        }

        impl $name {
            /// The value as sent to Meta.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $value, )+
                    Self::Other(s) => s,
                }
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = ::std::convert::Infallible;

            /// Case-insensitive; unknown values become `Other`.
            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                $(
                    if s.eq_ignore_ascii_case($value) {
                        return Ok(Self::$variant);
                    }
                )+
                Ok(Self::Other(s.to_owned()))
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> ::std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> ::std::result::Result<Self, D::Error> {
                let raw = <::std::string::String as ::serde::Deserialize>::deserialize(deserializer)?;
                // `from_str` is infallible.
                Ok(match raw.parse::<Self>() {
                    Ok(v) => v,
                    Err(never) => match never {},
                })
            }
        }
    };
}

pub(crate) use string_enum;
