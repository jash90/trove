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
///
/// Deliberately not renamed with the product. This string is a protocol
/// identity, not a label: it keys this device's token inside a file shared with
/// another consumer, and the vault mints and revokes against the same name.
/// Changing it would not rename anything — it would look up a token that does
/// not exist, and the vault would quietly read as unconfigured until someone
/// paired again.
pub const CONSUMER: &str = "clipboard-history";

/// Most bytes the identity file may carry. It holds one key, one URL and a
/// handful of tokens; anything larger is not that file, and reading it into
/// memory before finding out is how a wrong path becomes a memory problem.
const MAX_AGENT_FILE_BYTES: u64 = 64 * 1024;

/// The identity file as written. `token` is the single-consumer shorthand: a
/// device with one reader should not have to learn the map form.
/// One consumer's own paired identity: a keypair it generated itself and a token minted for it.
///
/// Preferred over the shared key below. A paired consumer holds a key nothing else has, so its
/// access can be withdrawn on its own — which the shared key, by being shared, cannot offer.
#[derive(serde::Deserialize, serde::Serialize)]
struct DeviceEntry {
    #[serde(rename = "privateJwk", alias = "private_jwk")]
    private_jwk: serde_json::Value,
    token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

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
    #[serde(default)]
    devices: BTreeMap<String, DeviceEntry>,
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

    // A consumer that paired for itself is answered from its own entry and never falls back to
    // the shared key: falling back would quietly restore the very coupling pairing removed.
    if let Some(entry) = file.devices.get(consumer) {
        let base_url = entry
            .url
            .as_deref()
            .or(file.url.as_deref())
            .and_then(non_blank)
            .ok_or(KeyvaultError::DeviceIdentityInvalid)?;
        let private_jwk = match &entry.private_jwk {
            serde_json::Value::Object(map) => {
                serde_json::to_string(map).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?
            }
            serde_json::Value::String(text) => text.clone(),
            _ => return Err(KeyvaultError::DeviceIdentityInvalid),
        };
        let config = KeyvaultConfig {
            base_url,
            token: non_blank(&entry.token).ok_or(KeyvaultError::DeviceIdentityMissing)?,
            private_jwk,
        };
        config.validate()?;
        return Ok(config);
    }

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

/// The vault address this device already knows, without needing a usable token.
///
/// Deliberately not [`load`]: that refuses when the consumer has no token, and a missing or
/// revoked token is exactly the situation in which someone needs to pair again. Requiring a
/// working credential in order to obtain a new one is a circle, and it is the circle that made
/// re-pairing impossible — pairing clears the address field, and the address was then only ever
/// read back from that field.
pub fn known_base_url(consumer: &str) -> Option<String> {
    let path = agent_file_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    known_base_url_in(&raw, consumer)
}

/// The parsing half, separated so tests need neither a filesystem nor the process environment.
pub fn known_base_url_in(raw: &str, consumer: &str) -> Option<String> {
    let file: AgentFile = serde_json::from_str(raw).ok()?;

    let non_blank = |value: &str| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };
    // This consumer's own address first: an install pointed at a second vault keeps pointing
    // there when it re-pairs, rather than silently drifting back to the shared one.
    file.devices
        .get(consumer)
        .and_then(|entry| entry.url.as_deref())
        .and_then(non_blank)
        .or_else(|| file.url.as_deref().and_then(non_blank))
        .filter(|url| crate::validate_base_url(url).is_ok())
}

/// Reads the identity file as a JSON object.
///
/// A missing file is an empty object, not an error: the first pairing on a fresh machine is
/// exactly the case pairing exists to serve.
fn read_root(path: &std::path::Path) -> Result<serde_json::Value, KeyvaultError> {
    let root = match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<serde_json::Value>(&raw)
            .map_err(|_| KeyvaultError::DeviceIdentityInvalid)?,
        Err(_) => serde_json::json!({}),
    };
    if !root.is_object() {
        return Err(KeyvaultError::DeviceIdentityInvalid);
    }
    Ok(root)
}

/// Writes the object back, owner-only, through a neighbour and a rename.
///
/// Never in place: a crash mid-write would truncate the file the MCP server reads too, taking out
/// a consumer that had nothing to do with this change.
fn write_root(path: &std::path::Path, root: &serde_json::Value) -> Result<(), KeyvaultError> {
    use std::io::Write;

    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
        set_owner_only(directory, 0o700);
    }
    let body =
        serde_json::to_string_pretty(root).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
    let temporary = path.with_extension("json.tmp-pairing");
    {
        let mut file =
            std::fs::File::create(&temporary).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
        set_owner_only(&temporary, 0o600);
        file.write_all(body.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
    }
    std::fs::rename(&temporary, path).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
    set_owner_only(path, 0o600);
    Ok(())
}

/// Records what a pairing produced, leaving everything else in the file as it was.
///
/// Read-modify-write rather than a rewrite: the file is shared with the MCP server, and pairing
/// one consumer must not disturb another's token or the shared key it still reads.
pub fn save_paired(
    consumer: &str,
    url: &str,
    private_jwk: &str,
    token: &str,
) -> Result<PathBuf, KeyvaultError> {
    let path = agent_file_path().ok_or(KeyvaultError::DeviceIdentityMissing)?;
    let mut root = read_root(&path)?;
    let key: serde_json::Value =
        serde_json::from_str(private_jwk).map_err(|_| KeyvaultError::InvalidPrivateKey)?;
    root["devices"][consumer] = serde_json::json!({
        "privateJwk": key,
        "token": token,
        "url": url,
    });
    write_root(&path, &root)?;
    Ok(path)
}

/// Drops one consumer's pairing, so it can pair again.
///
/// Only that consumer's entry. The file holds the shared key the MCP server reads and any other
/// consumer's pairing, and forgetting this one's is no reason to take theirs.
///
/// Local only: the device registered in the vault keeps existing, because revoking it is an
/// authenticated mutation and this application holds a token, not an account. The next pairing
/// offers to retire it, and the vault's Devices screen can do it by hand.
pub fn forget_paired(consumer: &str) -> Result<(), KeyvaultError> {
    let path = agent_file_path().ok_or(KeyvaultError::DeviceIdentityMissing)?;
    let root = read_root(&path)?;
    let body = serde_json::to_string(&root).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
    let updated = without_device(&body, consumer)?;
    let root: serde_json::Value =
        serde_json::from_str(&updated).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
    write_root(&path, &root)
}

/// The editing half, separated so tests need neither a filesystem nor the process environment.
///
/// A consumer that was not paired is not an error: Reset on an unpaired install should be a
/// no-op, not a refusal.
pub fn without_device(raw: &str, consumer: &str) -> Result<String, KeyvaultError> {
    let mut root: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| KeyvaultError::DeviceIdentityInvalid)?;
    if let Some(devices) = root.get_mut("devices").and_then(|d| d.as_object_mut()) {
        devices.remove(consumer);
    }
    serde_json::to_string(&root).map_err(|_| KeyvaultError::DeviceIdentityInvalid)
}

#[cfg(unix)]
fn set_owner_only(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path, _mode: u32) {}
