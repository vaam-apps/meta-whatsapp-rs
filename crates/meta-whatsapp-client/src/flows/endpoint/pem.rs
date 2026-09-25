//! Just enough PEM (RFC 7468) to load the business private key, without a
//! new dependency. Only unencrypted keys are accepted: aws-lc-rs exposes no
//! API for password-protected PKCS#8 or legacy `Proc-Type: 4,ENCRYPTED`
//! PKCS#1, so an encrypted key is rejected with instructions instead of a
//! half-working parser.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use meta_whatsapp_core::error::CryptoError;

/// Message for keys we cannot read because they are password-protected.
/// Meta's key-generation command (`openssl genrsa -des3`) produces exactly
/// such a key; the fix is the command Meta's own Java example gives.
pub(super) const ENCRYPTED_KEY: &str = "encrypted PEM private keys are not supported; decrypt it first: \
openssl pkcs8 -topk8 -nocrypt -in private.pem -out private_unencrypted_pkcs8.pem";

/// DER bytes of a private key, overwritten with zeros when dropped.
pub(super) struct Scrubbed(pub(super) Vec<u8>);

impl Drop for Scrubbed {
    fn drop(&mut self) {
        scrub(&mut self.0);
    }
}

/// Overwrite key material. `black_box` keeps the optimizer from eliding the
/// writes to a buffer that is about to be freed; this is best effort (no
/// `zeroize` dependency, and copies made by libraries are out of reach).
pub(super) fn scrub(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

/// A private key found in a PEM document.
pub(super) enum PemPrivateKey {
    /// `-----BEGIN PRIVATE KEY-----` (PKCS#8).
    Pkcs8(Scrubbed),
    /// `-----BEGIN RSA PRIVATE KEY-----` (PKCS#1), the form in Meta's
    /// examples.
    Pkcs1(Scrubbed),
}

/// Find the first PEM block and return its private key DER.
pub(super) fn parse_private_key(pem: &str) -> Result<PemPrivateKey, CryptoError> {
    const BEGIN: &str = "-----BEGIN ";
    const DASHES: &str = "-----";
    const NOT_PEM: &str = "expected a PEM private key (-----BEGIN PRIVATE KEY----- or -----BEGIN RSA PRIVATE KEY-----)";

    let start = pem.find(BEGIN).ok_or(CryptoError::InvalidKey(NOT_PEM))?;
    let after_begin = &pem[start + BEGIN.len()..];
    let label_len = after_begin
        .find(DASHES)
        .ok_or(CryptoError::InvalidKey(NOT_PEM))?;
    let label = &after_begin[..label_len];
    let rest = &after_begin[label_len + DASHES.len()..];
    let end = rest
        .find(&format!("-----END {label}-----"))
        .ok_or(CryptoError::InvalidKey(
            "PEM block has no matching END line",
        ))?;
    let body = &rest[..end];

    let pkcs1 = match label {
        "PRIVATE KEY" => false,
        "RSA PRIVATE KEY" => true,
        "ENCRYPTED PRIVATE KEY" => return Err(CryptoError::InvalidKey(ENCRYPTED_KEY)),
        "PUBLIC KEY" | "RSA PUBLIC KEY" => {
            return Err(CryptoError::InvalidKey(
                "this is a public key; the endpoint needs the private key",
            ));
        }
        _ => return Err(CryptoError::InvalidKey(NOT_PEM)),
    };
    // RFC 1421 headers (`Proc-Type: 4,ENCRYPTED`, `DEK-Info: …`) only appear
    // in legacy password-protected PKCS#1 keys; base64 never contains ':'.
    if body.contains(':') {
        return Err(CryptoError::InvalidKey(ENCRYPTED_KEY));
    }

    let encoded = Scrubbed(
        body.bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect::<Vec<u8>>(),
    );
    let der = Scrubbed(
        STANDARD
            .decode(&encoded.0)
            .map_err(|_| CryptoError::InvalidKey("PEM body is not valid base64"))?,
    );
    Ok(if pkcs1 {
        PemPrivateKey::Pkcs1(der)
    } else {
        PemPrivateKey::Pkcs8(der)
    })
}

/// Wrap DER in a PEM block with 64-character lines.
pub(super) fn encode(label: &str, der: &[u8]) -> String {
    let b64 = STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    // Base64 output is ASCII, so byte chunks are valid UTF-8.
    for line in b64.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
    out
}
