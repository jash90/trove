use aes_gcm::aead::{Aead, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::{BigUint, Oaep, RsaPrivateKey};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::{AgentEnvelope, KeyvaultError};

/// Most bytes a private JWK may carry: a 3072-bit key is around 2 KiB of
/// JSON, so anything past this bound is a pasted mistake, not a key.
pub const MAX_PRIVATE_JWK_BYTES: usize = 16 * 1024;

/// An RSA-OAEP private key, opened from the JWK this device keeps.
///
/// No `Debug` — this is the key that opens every secret in the vault.
pub struct PrivateKey(RsaPrivateKey);

#[derive(serde::Deserialize)]
struct PrivateJwk {
    n: String,
    e: String,
    d: String,
    p: String,
    q: String,
}

fn decode_component(value: &str) -> Option<BigUint> {
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(BigUint::from_bytes_be(&bytes))
}

fn parse_components(jwk_json: &str) -> Result<PrivateJwk, KeyvaultError> {
    let jwk: PrivateJwk =
        serde_json::from_str(jwk_json).map_err(|_| KeyvaultError::InvalidPrivateKey)?;
    for component in [&jwk.n, &jwk.e, &jwk.d, &jwk.p, &jwk.q] {
        decode_component(component).ok_or(KeyvaultError::InvalidPrivateKey)?;
    }
    Ok(jwk)
}

/// Checks the JWK is the five-component base64url JSON the decryptor reads.
///
/// Shape only: proving the components assemble into a working key happens in
/// [`parse_private_jwk`], which is what a copy action runs.
pub fn validate_private_jwk(jwk_json: &str) -> Result<(), KeyvaultError> {
    if jwk_json.len() > MAX_PRIVATE_JWK_BYTES {
        return Err(KeyvaultError::InvalidPrivateKey);
    }
    parse_components(jwk_json).map(|_| ())
}

/// Opens the private key from its JWK form (base64url `n`, `e`, `d`, `p`, `q`).
pub fn parse_private_jwk(jwk_json: &str) -> Result<PrivateKey, KeyvaultError> {
    if jwk_json.len() > MAX_PRIVATE_JWK_BYTES {
        return Err(KeyvaultError::InvalidPrivateKey);
    }
    let jwk = parse_components(jwk_json)?;
    let key = RsaPrivateKey::from_components(
        decode_component(&jwk.n).ok_or(KeyvaultError::InvalidPrivateKey)?,
        decode_component(&jwk.e).ok_or(KeyvaultError::InvalidPrivateKey)?,
        decode_component(&jwk.d).ok_or(KeyvaultError::InvalidPrivateKey)?,
        vec![
            decode_component(&jwk.p).ok_or(KeyvaultError::InvalidPrivateKey)?,
            decode_component(&jwk.q).ok_or(KeyvaultError::InvalidPrivateKey)?,
        ],
    )
    .map_err(|_| KeyvaultError::InvalidPrivateKey)?;
    Ok(PrivateKey(key))
}

/// Unwraps the AES key and opens the ciphertext.
///
/// The plaintext comes back zeroed on drop; it is the caller's job to keep it
/// out of logs and `Debug` impls on the way to wherever it is going.
pub fn decrypt_envelope(
    key: &PrivateKey,
    envelope: &AgentEnvelope,
) -> Result<Zeroizing<String>, KeyvaultError> {
    let mut rng = rsa::rand_core::OsRng;
    let raw_key = Zeroizing::new(
        key.0
            .decrypt_blinded(&mut rng, Oaep::new::<Sha256>(), &envelope.enc_key)
            .map_err(|_| KeyvaultError::DecryptFailed)?,
    );
    let cipher =
        aes_gcm::Aes256Gcm::new_from_slice(&raw_key).map_err(|_| KeyvaultError::DecryptFailed)?;
    let plaintext = cipher
        .decrypt((&envelope.iv).into(), envelope.ciphertext.as_ref())
        .map_err(|_| KeyvaultError::DecryptFailed)?;
    String::from_utf8(plaintext)
        .map(Zeroizing::new)
        .map_err(|_| KeyvaultError::DecryptFailed)
}
