//! The keyvault pane's backing: one client, configured from settings, whose
//! plaintext exists only between the decrypt and the clipboard write.
//!
//! The vault is zero-knowledge — its answers are sealed envelopes, and the
//! private key that opens them lives on this device. Everything here keeps it
//! that way: errors carry codes only, results carry metadata only, and the
//! one place plaintext appears arms the capture monitor first so a fetched key
//! cannot land in the history this application exists to keep private.

use std::sync::{Mutex, OnceLock};

use clipboard_keyvault::{
    KeyvaultClient, KeyvaultConfig, ReqwestSecretTransport, SecretTransport, decrypt_envelope,
    parse_private_jwk,
};
use tauri::{AppHandle, Manager, Runtime};

use crate::{commands::AppSettingsDto, state::AppState};

/// The vault rate-limits a token to sixty requests a minute; spacing this
/// application's reads a little past a second keeps a settings screen from
/// ever turning a 429 into the pane's only message.
const MIN_REQUEST_INTERVAL_MS: i64 = 1_050;

/// Reads the vault configuration out of the settings, or says it is absent.
///
/// A row that passed validation resolves here; the redundant check is for the
/// same reason every boundary here checks twice.
pub fn config_from(settings: &AppSettingsDto) -> Result<KeyvaultConfig, String> {
    let Some((base_url, token, private_jwk)) = settings.keyvault.resolved() else {
        return Err("keyvault_not_configured".to_owned());
    };
    let config = KeyvaultConfig {
        base_url,
        token,
        private_jwk,
    };
    config
        .validate()
        .map_err(|_| "keyvault_invalid_config".to_owned())?;
    Ok(config)
}

/// Spaces vault reads. A refusal does not move the clock, so a retry after a
/// denial is not punished for the denial's time.
pub fn throttle(now_ms: i64) -> Result<(), String> {
    static LAST_REQUEST_MS: OnceLock<Mutex<i64>> = OnceLock::new();
    let mut last = LAST_REQUEST_MS
        .get_or_init(|| Mutex::new(0))
        .lock()
        .expect("the vault throttle lock is only taken briefly");
    if now_ms.saturating_sub(*last) < MIN_REQUEST_INTERVAL_MS {
        return Err("keyvault_rate_limited".to_owned());
    }
    *last = now_ms;
    Ok(())
}

/// Lists the secrets the token may read — metadata only, nothing sealed.
pub async fn list_service(state: &AppState) -> Result<Vec<clipboard_keyvault::SecretRef>, String> {
    let store = state.store.clone();
    let settings = crate::commands::run_blocking("settings_unavailable", move || {
        crate::commands::get_settings_blocking(&store)
    })
    .await?;
    let config = config_from(&settings)?;
    let transport = ReqwestSecretTransport::new(&config).map_err(|error| error.to_string())?;
    KeyvaultClient::new(transport)
        .list()
        .await
        .map_err(|error| error.to_string())
}

/// Fetches one secret, opens it, and puts it on the clipboard.
///
/// Arming the suppression before the write is the same contract as copying an
/// entry back: the monitor cannot record this application's own clipboard
/// change — and a key landing in history would be exactly the leak this whole
/// module exists to prevent. The plaintext never crosses into the interface:
/// the caller learns only whether it worked.
pub async fn copy_secret_service<R: Runtime, T: SecretTransport>(
    app: &AppHandle<R>,
    transport: T,
    config: &KeyvaultConfig,
    slug: &str,
) -> Result<(), String> {
    let client = KeyvaultClient::new(transport);
    let (_name, envelope) = client
        .fetch_sealed(slug)
        .await
        .map_err(|error| error.to_string())?;
    let private_key = parse_private_jwk(&config.private_jwk).map_err(|error| error.to_string())?;
    let plaintext = decrypt_envelope(&private_key, &envelope).map_err(|error| error.to_string())?;
    if let Some(control) = app.try_state::<crate::monitor::MonitorControl>() {
        control.suppress_next_change(crate::commands::current_time_ms());
    }
    // Asked through try_state rather than `clipboard()`: the extension method
    // panics when the plugin is absent, and "the clipboard is unavailable" is
    // the honest answer a panic would swallow.
    match app.try_state::<tauri_plugin_clipboard_manager::Clipboard<R>>() {
        Some(clipboard) => clipboard
            .write_text(plaintext.as_str())
            .map_err(|_| "clipboard_unavailable".to_owned()),
        None => Err("clipboard_unavailable".to_owned()),
    }
}
