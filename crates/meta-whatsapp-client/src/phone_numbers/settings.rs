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
/// Every field is optional, `status` too although the schema requires it:
/// an answer that departs from the schema must not fail the whole
/// [`PhoneNumber::settings`](super::PhoneNumber::settings) read, local
/// storage included. `Debug` shows the status and the fingerprint, and of
/// the cloud key only its length ([`CloudEncryptionKey`]). `Serialize`
/// writes every field, as read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PayloadEncryption {
    /// Current status; `None` when the answer leaves it out (the schema
    /// requires it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PayloadEncryptionStatus>,
    /// Fingerprint of the client key set with
    /// [`PhoneNumber::enable_payload_encryption`](super::PhoneNumber::enable_payload_encryption)
    /// ("when encryption is enabled"). How Meta computes it is not
    /// documented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_encryption_key_fingerprint: Option<String>,
    /// Meta's key, "Cloud encryption key (when encryption is enabled)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_encryption_key: Option<CloudEncryptionKey>,
}

/// `cloud_encryption_key` of the `payload_encryption` answer: "Cloud
/// encryption key (when encryption is enabled)", the reference's whole
/// description.
///
/// The schema pairs it with the business's public key
/// ([`ClientEncryptionKey`]) and that key's fingerprint, so it is read as
/// Meta's public key, but the reference does not say so: it is never
/// printed. `Debug` shows only its length and there is no `Display`;
/// [`Self::as_str`] gives the value. `Serialize` and `Deserialize` are
/// transparent (the JSON string, as Meta sent it).
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct CloudEncryptionKey(String);

impl CloudEncryptionKey {
    /// The key, as Meta sent it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudEncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CloudEncryptionKey")
            .field(&KeyLength::of(&self.0))
            .finish()
    }
}

/// The public key Meta encrypts payloads to (`client_encryption_key` of
/// `payload_encryption`): "Base64-encoded public key for payload
/// encryption", the reference's whole description.
///
/// It is public key material, but it is handled conservatively:
/// [`Self::new`] checks it is base64 and not an obvious private key,
/// without echoing it in the error; `Debug` shows only its length, and
/// there is no `Display` or `Serialize`, so it cannot reach a log or a
/// JSON dump by accident. Which key type, and which encoding inside the
/// base64, Meta accepts is not documented, so nothing more is checked.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientEncryptionKey(String);

/// Base64 with the standard alphabet, strictly (no whitespace, no
/// base64url); the reference does not say whether the key is padded, so
/// both are accepted.
const KEY_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

impl ClientEncryptionKey {
    /// Check and wrap a base64-encoded public key: a [`ValidationError`]
    /// naming `client_encryption_key` (never the value) when it is
    /// empty, not base64 (the standard alphabet, padding optional,
    /// nothing stripped), or decodes to an obvious private key: a PEM
    /// `PRIVATE KEY` block, or DER whose outer `SEQUENCE` opens with a
    /// one-byte `INTEGER`, the version number PKCS#1, PKCS#8 and SEC1
    /// private keys start with. A public key's `SEQUENCE` opens with its
    /// algorithm (SPKI) or a modulus (PKCS#1 `RSAPublicKey`), and passes.
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        let invalid = |reason: &'static str| -> Result<Self> {
            Err(ValidationError::new("client_encryption_key", reason).into())
        };
        let Ok(decoded) = KEY_BASE64.decode(key.as_bytes()) else {
            return invalid("must be a base64-encoded public key");
        };
        if decoded.is_empty() {
            return invalid("must be a base64-encoded public key");
        }
        if looks_like_private_key(&decoded) {
            return invalid("is a private key: send the public key only");
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

/// A PEM private key block (`-----BEGIN PRIVATE KEY-----`, `RSA PRIVATE
/// KEY`, `EC PRIVATE KEY`, `ENCRYPTED PRIVATE KEY`), or DER whose outer
/// `SEQUENCE`'s first element is a one-byte `INTEGER` (a private key's
/// version).
fn looks_like_private_key(der_or_pem: &[u8]) -> bool {
    const PEM_MARK: &[u8] = b"PRIVATE KEY";
    if der_or_pem.windows(PEM_MARK.len()).any(|w| w == PEM_MARK) {
        return true;
    }
    let [0x30, len, rest @ ..] = der_or_pem else {
        return false;
    };
    // Skip the long form of the SEQUENCE's length (`0x81`..`0x84` then
    // that many bytes); the short form is the byte itself.
    let content = match *len {
        0x81..=0x84 => rest.get(usize::from(*len - 0x80)..),
        0x00..=0x7f => Some(rest),
        _ => None,
    };
    matches!(content, Some([0x02, 0x01, ..]))
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

    /// Throwaway keys made for this test only (`openssl genpkey`, P-256 and
    /// RSA-512), DER then base64: never used anywhere.
    const TEST_ONLY_EC_PKCS8_PRIVATE: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgSf8oaiQQKM18r4MCVxzPt2X7vF8YTVcEjnvvKOLZ1HehRANCAAQrlHtSa/O8I0TSG31o40dMHkTgSa8FNZPUVjaqoOY4Kt+TVqi56zz55tHHw0TZyuJ+geXo1xeH4TWnbL/2Lkuc";
    const TEST_ONLY_EC_SEC1_PRIVATE: &str = "MHcCAQEEIEn/KGokECjNfK+DAlccz7dl+7xfGE1XBI577yji2dR3oAoGCCqGSM49AwEHoUQDQgAEK5R7UmvzvCNE0ht9aONHTB5E4EmvBTWT1FY2qqDmOCrfk1aoues8+ebRx8NE2crifoHl6NcXh+E1p2y/9i5LnA==";
    const TEST_ONLY_RSA_PKCS1_PRIVATE: &str = "MIIBOwIBAAJBANqxAPfo3T0Zd3wPD6tri5+M7sPpmCxK9mGBW2DiK7mewjVZEp5JASSN57fWM8tt8lAnV19XvWPyJCrFjirVvc0CAwEAAQJAaY6AKikjCIfPTXkzlgkt9pAe+z84LUDtlXjLs4LXmhrriAlEeTsqRSGvK/sdeEiNozNE888DKjHM9UbDWjeznQIhAPY1AA2R+iCax/7WSccJleKqqDxJdvt8Mj3owJnZvcADAiEA42PThDsQL6SeCLBCzxb67wa9krBDzjyvGPeC1T+lKe8CIQDI8CKV8LCSUaCuJFfcQL96WcuqQE8CGq+qhT5Dkkjh3wIgKGkySvqwKrdnuoWQqfsch2Q8Rph6xGk2eQprvvg7oxMCIQDsowInxRO57nkfzwYdjLop4ZHQpdyvxvHujQdDC2L9GA==";
    /// The public halves: SPKI (P-256) and PKCS#1 `RSAPublicKey` (RSA-512).
    const EC_SPKI_PUBLIC: &str = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEK5R7UmvzvCNE0ht9aONHTB5E4EmvBTWT1FY2qqDmOCrfk1aoues8+ebRx8NE2crifoHl6NcXh+E1p2y/9i5LnA==";
    const RSA_PKCS1_PUBLIC: &str = "MEgCQQDasQD36N09GXd8Dw+ra4ufjO7D6ZgsSvZhgVtg4iu5nsI1WRKeSQEkjee31jPLbfJQJ1dfV71j8iQqxY4q1b3NAgMBAAE=";

    #[test]
    fn client_encryption_key_refuses_private_keys() {
        for public in [EC_SPKI_PUBLIC, RSA_PKCS1_PUBLIC] {
            assert_eq!(ClientEncryptionKey::new(public).unwrap().as_str(), public);
        }
        // The public key as base64 of its PEM passes too.
        let pem =
            format!("-----BEGIN PUBLIC KEY-----\n{EC_SPKI_PUBLIC}\n-----END PUBLIC KEY-----\n");
        assert!(ClientEncryptionKey::new(KEY_BASE64.encode(pem)).is_ok());

        let private_pem = |label: &str, body: &str| {
            KEY_BASE64.encode(format!(
                "-----BEGIN {label}-----\n{body}\n-----END {label}-----\n"
            ))
        };
        for private in [
            TEST_ONLY_EC_PKCS8_PRIVATE.to_owned(),
            TEST_ONLY_EC_SEC1_PRIVATE.to_owned(),
            TEST_ONLY_RSA_PKCS1_PRIVATE.to_owned(),
            private_pem("PRIVATE KEY", TEST_ONLY_EC_PKCS8_PRIVATE),
            private_pem("EC PRIVATE KEY", TEST_ONLY_EC_SEC1_PRIVATE),
            private_pem("RSA PRIVATE KEY", TEST_ONLY_RSA_PKCS1_PRIVATE),
        ] {
            let err = ClientEncryptionKey::new(private.clone()).unwrap_err();
            assert!(matches!(err, meta_whatsapp_core::Error::Validation(_)));
            let text = format!("{err} {err:?}");
            assert!(text.contains("private key"), "{text}");
            assert!(!text.contains(&private[..16]), "{text}");
        }
    }

    #[test]
    fn cloud_key_is_a_transparent_string() {
        let k: CloudEncryptionKey = serde_json::from_str("\"Q0xPVUQ=\"").unwrap();
        assert_eq!(k.as_str(), "Q0xPVUQ=");
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"Q0xPVUQ=\"");
        assert_eq!(format!("{k:?}"), "CloudEncryptionKey(<8 bytes>)");
    }

    #[test]
    fn keys_show_only_their_length_in_debug() {
        let key = ClientEncryptionKey::new("TUlJQklqQU5CZ2txaGtpRzl3MEJB").unwrap();
        assert_eq!(format!("{key:?}"), "ClientEncryptionKey(<28 bytes>)");
        let read = PayloadEncryption {
            status: Some(PayloadEncryptionStatus::Enabled),
            client_encryption_key_fingerprint: Some("FP-1".into()),
            cloud_encryption_key: Some(CloudEncryptionKey("Q0xPVURLRVk=".into())),
        };
        let dbg = format!("{read:?}");
        assert_eq!(
            dbg,
            "PayloadEncryption { status: Some(Enabled), client_encryption_key_fingerprint: Some(\"FP-1\"), cloud_encryption_key: Some(CloudEncryptionKey(<12 bytes>)) }"
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
