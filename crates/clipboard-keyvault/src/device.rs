//! The device's shared agent identity: one file, read by every consumer.
//!
//! The private key used to be pasted into each consumer separately — into this
//! application's settings row, and into the MCP server's environment. Two
//! copies made a rotation two edits, and the copy you forgot did not announce
//! itself: it failed as `keyvault_decrypt_failed` against every re-sealed
//! envelope, a message about the envelope when the fault was the key. So the
//! key lives in one place now and consumers read it from there.
//!
//! The token is deliberately not shared the same way. A token is per-consumer
//! so one can be revoked without taking the others down with it, which is why
//! `tokens` is a map keyed by consumer name rather than a single field.
//!
//! ```json
//! // ~/.config/keyvault/agent.json, mode 600
//! {
//!   "url": "https://<deployment>.convex.site",
//!   "privateJwk": { "kty": "RSA", "n": "...", "e": "...", "d": "...", "p": "...", "q": "..." },
//!   "tokens": { "mcp": "kv_...", "clipboard-history": "kv_..." }
//! }
//! ```

use std::{collections::BTreeMap, path::PathBuf};

use crate::{KeyvaultConfig, KeyvaultError};

/// This application's name in the `tokens` map.
pub const CONSUMER: &str = "clipboard-history";

/// Most bytes the identity file may carry. It holds one key, one URL and a
/// handful of tokens; anything larger is not that file, and reading it into
/// memory before finding out is how a wrong path becomes a memory problem.
const MAX_AGENT_FILE_BYTES: u64 = 64 * 1024;

/// The identity file as written. `token` is the single-consumer shorthand: a
/// device with one reader should not have to learn the map form.
#[derive(serde::Deserialize)]
struct AgentFile {
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "privateJwk", alias = "private_jwk")]
    private_jwk: Option<serde_json::Value>,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
    #[serde(default)]
    token: Option<String>,
}

/// Where the identity file lives: `KEYVAULT_AGENT_FILE`, else the fixed
/// `~/.config/keyvault/agent.json`.
///
/// Deliberately not derived from `KEYVAULT_SECRET_DIR`. That names the scratch
/// directory the MCP server materializes decrypted secrets into, which a
/// device may well point at `/tmp` precisely so it gets wiped. The identity is
/// the opposite kind of thing, and following the scratch directory would hide
/// it from any consumer that moved one knob and not the other.
pub fn agent_file_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KEYVAULT_AGENT_FILE") {
        return Some(PathBuf::from(path));
    }
    Some(
        PathBuf::from(std::env::var_os("HOME")?)
            .join(".config")
            .join("keyvault")
            .join("agent.json"),
    )
}

/// Reads this application's vault configuration from the device identity file.
///
/// A missing file and a malformed one are different answers on purpose. The
/// first is the ordinary unconfigured state, which the settings pane explains;
/// the second is a mistake someone made in an editor, and saying so is the
/// only way they will find it.
pub fn load(consumer: &str) -> Result<KeyvaultConfig, KeyvaultError> {
    let path = agent_file_path().ok_or(KeyvaultError::DeviceIdentityMissing)?;
    let metadata = std::fs::metadata(&path).map_err(|_| KeyvaultError::DeviceIdentityMissing)?;
    if metadata.len() > MAX_AGENT_FILE_BYTES {
        return Err(KeyvaultError::DeviceIdentityInvalid);
    }
    let raw = std::fs::read_to_string(&path).map_err(|_| KeyvaultError::DeviceIdentityMissing)?;
    parse(&raw, consumer)
}

/// The parsing half, separated so tests need no filesystem.
pub fn parse(raw: &str, consumer: &str) -> Result<KeyvaultConfig, KeyvaultError> {
    let file: AgentFile =
        serde_json::from_str(raw).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;

    let non_blank = |value: &str| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };

    let base_url = file
        .url
        .as_deref()
        .and_then(non_blank)
        .ok_or(KeyvaultError::DeviceIdentityInvalid)?;

    // This consumer's own token, else the single-consumer shorthand. A device
    // that lists other consumers but not this one is configured, just not for
    // us — which is the unconfigured state, not a malformed file.
    let token = file
        .tokens
        .get(consumer)
        .map(String::as_str)
        .or(file.token.as_deref())
        .and_then(non_blank)
        .ok_or(KeyvaultError::DeviceIdentityMissing)?;

    // The file holds a real JSON object; the environment has always held a
    // JSON *string*. Both arrive here, because a consumer moving between them
    // should not have to care which it got.
    let private_jwk = match file.private_jwk {
        Some(serde_json::Value::Object(map)) => {
            serde_json::to_string(&map).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?
        }
        Some(serde_json::Value::String(text)) => text,
        _ => return Err(KeyvaultError::DeviceIdentityInvalid),
    };

    let config = KeyvaultConfig {
        base_url,
        token,
        private_jwk,
    };
    // Validate here rather than at first use: a bad identity file should name
    // itself when it is read, not when someone clicks copy on a secret.
    config.validate()?;
    Ok(config)
}
