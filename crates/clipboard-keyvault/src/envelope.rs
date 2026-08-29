use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::KeyvaultError;

/// The most envelope bytes accepted, mirroring the vault's own sealing bound.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;

/// The hybrid envelope the vault returns instead of the secret: an
/// RSA-OAEP-wrapped one-shot AES-256 key, its 12-byte IV, and the AES-GCM
/// ciphertext with the tag appended, exactly as Web Crypto emits it.
///
/// No `Debug`: this is key material, not diagnostics.
pub struct AgentEnvelope {
    pub(crate) enc_key: Vec<u8>,
    pub(crate) iv: [u8; 12],
    pub(crate) ciphertext: Vec<u8>,
}

#[derive(serde::Deserialize)]
struct EnvelopeWire {
    v: i64,
    #[serde(rename = "encKey")]
    enc_key: String,
    iv: String,
    ct: String,
}

/// Parses and bounds-checks one envelope JSON document.
///
/// The version check is load-bearing, not defensive: a future envelope shape
/// must fail closed here rather than decrypt into nonsense.
pub fn parse_envelope(json: &str) -> Result<AgentEnvelope, KeyvaultError> {
    if json.len() > MAX_ENVELOPE_BYTES {
        return Err(KeyvaultError::EnvelopeTooLarge);
    }
    let wire: EnvelopeWire =
        serde_json::from_str(json).map_err(|_| KeyvaultError::EnvelopeInvalid)?;
    if wire.v != 1 {
        return Err(KeyvaultError::EnvelopeUnsupportedVersion);
    }
    let enc_key = STANDARD
        .decode(wire.enc_key)
        .map_err(|_| KeyvaultError::EnvelopeInvalid)?;
    let iv = STANDARD
        .decode(wire.iv)
        .map_err(|_| KeyvaultError::EnvelopeInvalid)?;
    let ciphertext = STANDARD
        .decode(wire.ct)
        .map_err(|_| KeyvaultError::EnvelopeInvalid)?;
    let iv: [u8; 12] = iv.try_into().map_err(|_| KeyvaultError::EnvelopeInvalid)?;
    Ok(AgentEnvelope {
        enc_key,
        iv,
        ciphertext,
    })
}
