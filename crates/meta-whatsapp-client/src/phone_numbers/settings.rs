//! Phone number settings that belong to the number's lifecycle: local
//! storage (data-at-rest region) and the identity change check. Calling
//! settings share the endpoint but are typed in [`crate::calling`].

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use meta_whatsapp_core::error::ValidationError;

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
/// `calling` and `payload_encryption` are kept raw: calling is typed in
/// [`crate::calling`], and payload encryption has no guide page with an
/// example to test against yet.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PhoneNumberSettings {
    /// Local storage configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_configuration: Option<StorageConfiguration>,
    /// Calling settings, untyped here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calling: Option<serde_json::Value>,
    /// Payload encryption settings, untyped here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_encryption: Option<serde_json::Value>,
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
    }
}
