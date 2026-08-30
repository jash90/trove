#![forbid(unsafe_code)]

//! Client for the keyvault this machine's owner already runs: a zero-knowledge
//! vault whose REST API hands out secrets as sealed envelopes, not plaintext.
//!
//! The bearer token authenticates; the RSA private key this device holds is
//! what actually opens anything. Both are read from the device's shared
//! identity file (see [`device`]) rather than pasted into this application,
//! so a key rotation is one edit instead of one per consumer. Plaintext exists only inside
//! [`decrypt::decrypt_envelope`]'s return value, which zeroes itself on drop —
//! it never enters a `Debug` impl, a log line, or an error message.

mod client;
mod config;
mod decrypt;
mod device;
mod envelope;
mod http;
mod pairing;

#[cfg(test)]
mod tests;

use thiserror::Error;

pub use client::{KeyvaultClient, SecretRef};
pub use config::{KeyvaultConfig, MAX_TOKEN_BYTES, validate_base_url, validate_token};
pub use decrypt::{PrivateKey, decrypt_envelope, parse_private_jwk};
pub use device::{CONSUMER, agent_file_path, load as load_device_identity, save_paired};
pub use envelope::{AgentEnvelope, MAX_ENVELOPE_BYTES};
pub use http::{ReqwestSecretTransport, SecretResponse, SecretTransport};
pub use pairing::{
    DeviceKey, PairingOutcome, PairingStart, claim as claim_pairing, generate_device_key,
    page_url as pairing_page_url, start as start_pairing,
};

/// Every failure this crate can report, as a stable code.
///
/// The codes are the whole message: a body fragment from the vault, a slug, or
/// a fragment of the private key would all be exactly the leak this crate
/// exists to prevent.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum KeyvaultError {
    #[error("keyvault_invalid_url")]
    InvalidUrl,
    #[error("keyvault_invalid_token")]
    InvalidToken,
    #[error("keyvault_invalid_private_key")]
    InvalidPrivateKey,
    /// No identity file on this device, or none naming this consumer: the
    /// ordinary unconfigured state, which the settings pane explains.
    #[error("keyvault_device_identity_missing")]
    DeviceIdentityMissing,
    /// An identity file that exists but does not parse. Distinct from missing
    /// because a typo in an editor deserves to be named as one.
    #[error("keyvault_device_identity_invalid")]
    DeviceIdentityInvalid,
    /// The deployment has not published where its web interface lives, so there is nowhere to
    /// send someone to approve a pairing.
    #[error("keyvault_pairing_page_unknown")]
    PairingPageUnknown,
    /// The approved pairing handed back something that is not a complete identity. Distinct from
    /// a bad response in general because it points at the vault's own page, not at the network.
    #[error("keyvault_pairing_payload_invalid")]
    PairingPayloadInvalid,
    #[error("keyvault_invalid_slug")]
    InvalidSlug,
    #[error("keyvault_unauthorized")]
    Unauthorized,
    #[error("keyvault_agent_access_disabled")]
    AgentAccessDisabled,
    #[error("keyvault_not_found")]
    NotFound,
    #[error("keyvault_rate_limited")]
    RateLimited,
    #[error("keyvault_envelope_invalid")]
    EnvelopeInvalid,
    #[error("keyvault_envelope_unsupported_version")]
    EnvelopeUnsupportedVersion,
    #[error("keyvault_envelope_too_large")]
    EnvelopeTooLarge,
    #[error("keyvault_decrypt_failed")]
    DecryptFailed,
    #[error("keyvault_transport_failed")]
    Transport,
    #[error("keyvault_bad_response")]
    BadResponse,
}
