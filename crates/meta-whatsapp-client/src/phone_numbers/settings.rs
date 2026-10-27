//! Phone number settings that belong to the number's lifecycle: local
//! storage (data-at-rest region), the identity change check and payload
//! encryption. Calling settings share the endpoint but are typed in
//! [`crate::calling`].

use std::fmt;

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::templates::macros::string_enum;

/// Country where message data is stored at rest (local storage).
///
/// The documented set (`business-phone-numbers/registration` and the
/// `phone-number-registration` reference agree on it). [`Self::Other`]
/// carries a code Meta adds later; it must still be a two-letter uppercase
/// ISO 3166 code, which [`Self::validate`] checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DataLocalizationRegion {
    /// Australia.
    Au,
    /// Indonesia.
    Id,
    /// India.
    In,
    /// Japan.
    Jp,
    /// Singapore.
    Sg,
    /// South Korea.
    Kr,
    /// EU (Germany).
    De,
    /// Switzerland.
    Ch,
    /// United Kingdom.
    Gb,
    /// Brazil.
    Br,
    /// Bahrain.
    Bh,
    /// South Africa.
    Za,
    /// United Arab Emirates.
    Ae,
    /// Canada.
    Ca,
    /// A code not in the documented list.
    Other(String),
}

impl DataLocalizationRegion {
    /// The ISO 3166 code Meta expects.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Au => "AU",
            Self::Id => "ID",
            Self::In => "IN",
            Self::Jp => "JP",
            Self::Sg => "SG",
            Self::Kr => "KR",
            Self::De => "DE",
            Self::Ch => "CH",
            Self::Gb => "GB",
            Self::Br => "BR",
            Self::Bh => "BH",
            Self::Za => "ZA",
            Self::Ae => "AE",
            Self::Ca => "CA",
            Self::Other(code) => code,
        }
    }

    /// Map a code to a variant (unknown codes become [`Self::Other`]).
    pub fn from_code(code: &str) -> Self {
        match code {
            "AU" => Self::Au,
            "ID" => Self::Id,
            "IN" => Self::In,
            "JP" => Self::Jp,
            "SG" => Self::Sg,
            "KR" => Self::Kr,
            "DE" => Self::De,
            "CH" => Self::Ch,
            "GB" => Self::Gb,
            "BR" => Self::Br,
            "BH" => Self::Bh,
            "ZA" => Self::Za,
            "AE" => Self::Ae,
            "CA" => Self::Ca,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Check the documented format: a two-letter uppercase country code.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let code = self.as_str();
        if code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase()) {
            Ok(())
        } else {
            Err(ValidationError::new(
                "data_localization_region",
                "must be a two-letter uppercase ISO 3166 country code",
            ))
        }
    }
}

impl Serialize for DataLocalizationRegion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DataLocalizationRegion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(|s| Self::from_code(&s))
    }
}

/// `GET /{PHONE_NUMBER_ID}/settings`, the parts this module owns.
///
/// `calling` is kept raw: it is typed in [`crate::calling`]
/// ([`crate::calling::Calling::settings`]).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PhoneNumberSettings {
    /// Local storage configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_configuration: Option<StorageConfiguration>,
    /// Calling settings, untyped here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calling: Option<serde_json::Value>,
    /// Payload encryption settings (`PayloadEncryptionSettingsResponse`);
    /// optional in the reference's schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_encryption: Option<PayloadEncryption>,
}

string_enum! {
    /// Whether payload encryption is on (`status` of `payload_encryption`,
    /// `settings-api`: `enabled`, `disabled`).
    pub enum PayloadEncryptionStatus {
        /// Payload encryption on.
        Enabled => "enabled",
        /// Payload encryption off.
        Disabled => "disabled",
    }
}

/// `payload_encryption` object read back from `GET
/// /{PHONE_NUMBER_ID}/settings` (`PayloadEncryptionSettingsResponse` in
/// `settings-api`; the page has no example, so this follows its schema).
///
/// `Debug` shows the status and the fingerprint, and of the cloud key only
/// its length: the reference does not say whether that key is public, so
/// it is kept out of logs. `Serialize` writes every field, as read.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PayloadEncryption {
    /// Current status (required in the schema).
    pub status: PayloadEncryptionStatus,
    /// Fingerprint of the client key set with
    /// [`PhoneNumber::enable_payload_encryption`](super::PhoneNumber::enable_payload_encryption)
    /// ("when encryption is enabled"). How Meta computes it is not
    /// documented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_encryption_key_fingerprint: Option<String>,
    /// Meta's key, "Cloud encryption key (when encryption is enabled)": the
    /// reference says no more about its format or use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_encryption_key: Option<String>,
}

impl fmt::Debug for PayloadEncryption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PayloadEncryption")
            .field("status", &self.status)
            .field(
                "client_encryption_key_fingerprint",
                &self.client_encryption_key_fingerprint,
            )
            .field(
                "cloud_encryption_key",
                &self.cloud_encryption_key.as_deref().map(KeyLength::of),
            )
            .finish()
    }
}

/// The public key Meta encrypts payloads to (`client_encryption_key` of
/// `payload_encryption`): "Base64-encoded public key for payload
/// encryption", the reference's whole description.
///
/// It is public key material, but it is handled conservatively:
/// [`Self::new`] checks it is base64 (the standard alphabet, padding
/// optional) without echoing it in the error, `Debug` shows only its
/// length, and there is no `Display` or `Serialize`, so it cannot reach a
/// log or a JSON dump by accident. Which key type, and which encoding
/// inside the base64, Meta accepts is not documented, so nothing more is
/// checked here.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientEncryptionKey(String);

/// Base64 with the standard alphabet; the reference does not say whether
/// the key is padded, so both are accepted.
const KEY_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

impl ClientEncryptionKey {
    /// Check and wrap a base64-encoded public key. Empty or not base64: a
    /// [`ValidationError`] naming `client_encryption_key` (never the
    /// value).
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.is_empty() || KEY_BASE64.decode(key.as_bytes()).is_err() {
            return Err(ValidationError::new(
                "client_encryption_key",
                "must be a base64-encoded public key",
            )
            .into());
        }
        Ok(Self(key))
    }

    /// The key, as sent to Meta.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClientEncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ClientEncryptionKey")
            .field(&KeyLength::of(&self.0))
            .finish()
    }
}

/// What `Debug` prints of a key: its length.
struct KeyLength(usize);

impl KeyLength {
    fn of(key: &str) -> Self {
        Self(key.len())
    }
}

impl fmt::Debug for KeyLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// `storage_configuration` object.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct StorageConfiguration {
    /// Whether in-country storage is on.
    pub status: StorageStatus,
    /// Region, when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_localization_region: Option<DataLocalizationRegion>,
}

/// Local storage status.
///
/// `local-storage` (guide, with examples) uses
/// `IN_COUNTRY_STORAGE_ENABLED`/`IN_COUNTRY_STORAGE_DISABLED`; the
/// `settings-api` reference lists `default`/`in_country_storage_enabled` in
/// lowercase. Requests follow the guide's examples; parsing accepts both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[non_exhaustive]
pub enum StorageStatus {
    /// Data at rest stays in `data_localization_region`.
    #[serde(
        rename = "IN_COUNTRY_STORAGE_ENABLED",
        alias = "in_country_storage_enabled"
    )]
    InCountryStorageEnabled,
    /// Local storage off.
    #[serde(
        rename = "IN_COUNTRY_STORAGE_DISABLED",
        alias = "in_country_storage_disabled"
    )]
    InCountryStorageDisabled,
    /// Meta's default storage (reference wording).
    #[serde(rename = "default", alias = "DEFAULT")]
    Default,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

#[derive(Serialize)]
pub(crate) struct StorageSettingsRequest<'a> {
    pub(crate) storage_configuration: StorageConfigurationRequest<'a>,
}

#[derive(Serialize)]
pub(crate) struct StorageConfigurationRequest<'a> {
    pub(crate) status: StorageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data_localization_region: Option<&'a DataLocalizationRegion>,
}

/// Body of the identity change check setting. The field name follows the
/// guide's example (`business-phone-numbers/phone-numbers`, "Identity
/// change check"): `enable_identity_key_check`. The `settings-api`
/// reference calls it `enabled`; the example wins, per the docs rule.
#[derive(Serialize)]
pub(crate) struct IdentitySettingsRequest {
    pub(crate) user_identity_change: IdentityChange,
}

#[derive(Serialize)]
pub(crate) struct IdentityChange {
    pub(crate) enable_identity_key_check: bool,
}

/// Body of the payload encryption setting
/// (`PayloadEncryptionSettingsRequest`, `settings-api`; schema only).
#[derive(Serialize)]
pub(crate) struct PayloadEncryptionSettingsRequest<'a> {
    pub(crate) payload_encryption: PayloadEncryptionRequest<'a>,
}

/// `PayloadEncryptionSettings`: `status` is required, the key "required
/// when enabling encryption".
#[derive(Serialize)]
pub(crate) struct PayloadEncryptionRequest<'a> {
    pub(crate) status: PayloadEncryptionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) client_encryption_key: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_round_trips_and_validates() {
        let r: DataLocalizationRegion = serde_json::from_str("\"BR\"").unwrap();
        assert_eq!(r, DataLocalizationRegion::Br);
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"BR\"");
        let other: DataLocalizationRegion = serde_json::from_str("\"FR\"").unwrap();
        assert_eq!(other, DataLocalizationRegion::Other("FR".into()));
        assert!(other.validate().is_ok());
        assert!(
            DataLocalizationRegion::Other("fr".into())
                .validate()
                .is_err()
        );
        assert!(
            DataLocalizationRegion::Other("FRA".into())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn parses_local_storage_settings_example() {
        // local-storage, "Get local storage settings".
        let v = r#"{"storage_configuration": {"status": "IN_COUNTRY_STORAGE_ENABLED", "data_localization_region": "BR"}}"#;
        let s: PhoneNumberSettings = serde_json::from_str(v).unwrap();
        let sc = s.storage_configuration.unwrap();
        assert_eq!(sc.status, StorageStatus::InCountryStorageEnabled);
        assert_eq!(
            sc.data_localization_region,
            Some(DataLocalizationRegion::Br)
        );
        // Reference casing parses too.
        let v =
            r#"{"storage_configuration": {"status": "default"}, "calling": {"status": "enabled"}}"#;
        let s: PhoneNumberSettings = serde_json::from_str(v).unwrap();
        assert_eq!(
            s.storage_configuration.unwrap().status,
            StorageStatus::Default
        );
        assert!(s.calling.is_some());
        assert_eq!(s.payload_encryption, None);
    }

    #[test]
    fn client_encryption_key_must_be_base64_and_is_never_echoed() {
        for good in [
            "TUlJQklqQU5CZ2txaGtpRzl3MEJBUUVGQUFPQ0FROEFNSUlCQ2dLQ0FRRUE=",
            "TUlJ",
            "TUk",
        ] {
            assert_eq!(ClientEncryptionKey::new(good).unwrap().as_str(), good);
        }
        for bad in [
            "",
            "-----BEGIN PUBLIC KEY-----",
            "TUlJ QklqQU5", // whitespace
            "TUlJ_-Ba",     // the URL-safe alphabet
            "TUlJQ===",
        ] {
            let err = ClientEncryptionKey::new(bad).unwrap_err();
            assert!(
                matches!(err, meta_whatsapp_core::Error::Validation(_)),
                "{bad:?}"
            );
            let text = err.to_string();
            assert!(text.contains("`client_encryption_key`"), "{text}");
            assert!(bad.len() < 4 || !text.contains(bad), "{text}");
        }
    }

    #[test]
    fn keys_show_only_their_length_in_debug() {
        let key = ClientEncryptionKey::new("TUlJQklqQU5CZ2txaGtpRzl3MEJB").unwrap();
        assert_eq!(format!("{key:?}"), "ClientEncryptionKey(<28 bytes>)");
        let read = PayloadEncryption {
            status: PayloadEncryptionStatus::Enabled,
            client_encryption_key_fingerprint: Some("FP-1".into()),
            cloud_encryption_key: Some("Q0xPVURLRVk=".into()),
        };
        let dbg = format!("{read:?}");
        assert_eq!(
            dbg,
            "PayloadEncryption { status: Enabled, client_encryption_key_fingerprint: Some(\"FP-1\"), cloud_encryption_key: Some(<12 bytes>) }"
        );
        let settings = PhoneNumberSettings {
            storage_configuration: None,
            calling: None,
            payload_encryption: Some(read),
        };
        assert!(!format!("{settings:?}").contains("Q0xPVURLRVk="));
    }

    #[test]
    fn payload_encryption_status_keeps_unknown_values() {
        let s: PayloadEncryptionStatus = serde_json::from_str("\"disabled\"").unwrap();
        assert_eq!(s, PayloadEncryptionStatus::Disabled);
        let s: PayloadEncryptionStatus = serde_json::from_str("\"ENABLED\"").unwrap();
        assert_eq!(s, PayloadEncryptionStatus::Enabled);
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"enabled\"");
        let s: PayloadEncryptionStatus = serde_json::from_str("\"rotating\"").unwrap();
        assert_eq!(s, PayloadEncryptionStatus::Other("rotating".into()));
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"rotating\"");
    }
}
