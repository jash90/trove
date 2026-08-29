#![forbid(unsafe_code)]

//! Client for the keyvault this machine's owner already runs: a zero-knowledge
//! vault whose REST API hands out secrets as sealed envelopes, not plaintext.
//!
//! The bearer token authenticates; the RSA private key this device holds is
//! what actually opens anything. Plaintext exists only inside
//! [`decrypt::decrypt_envelope`]'s return value, which zeroes itself on drop —
//! it never enters a `Debug` impl, a log line, or an error message.

mod client;
mod config;
mod decrypt;
mod envelope;
mod http;

#[cfg(test)]
mod tests;

use thiserror::Error;

pub use client::{KeyvaultClient, SecretRef};
pub use config::{KeyvaultConfig, MAX_TOKEN_BYTES};
pub use decrypt::{PrivateKey, decrypt_envelope, parse_private_jwk};
pub use envelope::{AgentEnvelope, MAX_ENVELOPE_BYTES};
pub use http::{ReqwestSecretTransport, SecretResponse, SecretTransport};

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
