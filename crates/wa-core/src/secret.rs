//! Secret-bearing strings. `Debug` never prints the value; reading it takes
//! an explicit `expose_secret()`.

use std::fmt;

use secrecy::{ExposeSecret, SecretSlice, SecretString};

macro_rules! secret_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone)]
        pub struct $name(SecretString);

        impl $name {
            /// Wrap a secret value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(SecretString::from(value.into()))
            }

            /// Read the secret. Keep the borrow short; never log it.
            pub fn expose_secret(&self) -> &str {
                self.0.expose_secret()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self::new(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self::new(s)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                String::deserialize(d).map(Self::new)
            }
        }
    };
}

secret_type!(
    /// A Graph API access token: system user, business integration system
    /// user (from Embedded Signup), user, or app token (`app_id|app_secret`).
    /// Opaque; never parse it.
    AccessToken
);
secret_type!(
    /// The Meta app secret. Signs webhooks (`X-Hub-Signature-256`) and is
    /// needed to exchange Embedded Signup codes for business tokens.
    AppSecret
);
secret_type!(
    /// The string you typed into the dashboard's **Verify token** field.
    VerifyToken
);

/// Secret key material as bytes (OTP pepper, vault keys). Zeroized on
/// drop; `Debug` is redacted; reading takes `expose_secret()`.
#[derive(Clone)]
pub struct SecretBytes(SecretSlice<u8>);

impl SecretBytes {
    /// Wrap secret bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(SecretSlice::from(bytes.into()))
    }

    /// Read the secret. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &[u8] {
        self.0.expose_secret()
    }

    /// Length in bytes (not secret).
    pub fn len(&self) -> usize {
        self.0.expose_secret().len()
    }

    /// Whether there are no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}

impl AccessToken {
    /// An app access token, `app_id|app_secret`, for endpoints that require
    /// one (e.g. `debug_token`, app subscriptions).
    pub fn app_token(app_id: &crate::ids::AppId, secret: &AppSecret) -> Self {
        Self::new(format!("{app_id}|{}", secret.expose_secret()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let t = AccessToken::new("EAAJBsecret");
        assert_eq!(format!("{t:?}"), "AccessToken([REDACTED])");
        assert_eq!(t.expose_secret(), "EAAJBsecret");
    }

    #[test]
    fn secret_bytes_redact_and_clone() {
        let b = SecretBytes::new(vec![1u8, 2, 3]);
        let c = b.clone();
        assert_eq!(format!("{c:?}"), "SecretBytes([REDACTED])");
        assert_eq!(c.expose_secret(), &[1, 2, 3]);
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn app_token_format() {
        let t = AccessToken::app_token(&"123".into(), &AppSecret::new("s3cr3t"));
        assert_eq!(t.expose_secret(), "123|s3cr3t");
    }
}
