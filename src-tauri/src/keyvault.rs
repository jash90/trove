//! The keyvault pane's backing: one client, configured from settings, whose
//! plaintext exists only between the decrypt and the clipboard write.
//!
//! The vault is zero-knowledge — its answers are sealed envelopes, and the
//! private key that opens them lives on this device, in the identity file
//! every consumer on the machine shares rather than in this application's own
//! settings row. Everything here keeps it
//! that way: errors carry codes only, results carry metadata only, and the
//! one place plaintext appears arms the capture monitor first so a fetched key
//! cannot land in the history this application exists to keep private.

use std::sync::{Mutex, OnceLock};

use clipboard_keyvault::{
    KeyvaultClient, KeyvaultConfig, ReqwestSecretTransport, SecretTransport, decrypt_envelope,
    parse_private_jwk,
};
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

/// The vault rate-limits a token to sixty requests a minute; spacing this
/// application's reads a little past a second keeps a settings screen from
/// ever turning a 429 into the pane's only message.
const MIN_REQUEST_INTERVAL_MS: i64 = 1_050;

/// Reads the vault configuration: the device's shared identity file, with
/// whatever this install overrides laid over it.
///
/// The private key is only ever the device's. There is deliberately no way to
/// override it from the settings — a second copy of the key is the thing this
/// arrangement exists to stop, and the copy you forget at rotation is the one
/// that fails as a decrypt error pointing at the wrong thing.
///
/// A missing identity is the ordinary unconfigured state and says so. A
/// malformed one is a different answer, because a typo deserves to be named.
pub fn config_from() -> Result<KeyvaultConfig, String> {
    let config = match clipboard_keyvault::load_device_identity(clipboard_keyvault::CONSUMER) {
        Ok(config) => config,
        Err(clipboard_keyvault::KeyvaultError::DeviceIdentityMissing) => {
            return Err("keyvault_not_configured".to_owned());
        }
        Err(error) => return Err(error.to_string()),
    };

    // The identity is the whole configuration. Settings used to be able to override the URL and
    // the token, and every fault in this feature came through that door: a revoked token left in
    // the row outranked a working pairing and produced a 401, and a web address typed into the
    // URL sent reads to a host that answers 405. Both were invited by fields that looked like
    // configuration and behaved like a trap. Pointing an install at a different vault is what
    // pairing is for, and the identity file is where a person edits one by hand.
    config
        .validate()
        .map_err(|_| "keyvault_invalid_config".to_owned())?;
    Ok(config)
}

/// Spaces vault reads. A refusal does not move the clock, so a retry after a
/// denial is not punished for the denial's time. A backward wall-clock step
/// (NTP correction, a manual change) also passes: clamping it to zero would
/// freeze every read until real time passed the stale mark again.
pub fn throttle(now_ms: i64) -> Result<(), String> {
    static LAST_REQUEST_MS: OnceLock<Mutex<i64>> = OnceLock::new();
    let mut last = LAST_REQUEST_MS
        .get_or_init(|| Mutex::new(0))
        .lock()
        .expect("the vault throttle lock is only taken briefly");
    let elapsed = now_ms - *last;
    if (0..MIN_REQUEST_INTERVAL_MS).contains(&elapsed) {
        return Err("keyvault_rate_limited".to_owned());
    }
    *last = now_ms;
    Ok(())
}

/// Lists the secrets the token may read — metadata only, nothing sealed.
pub async fn list_service() -> Result<Vec<clipboard_keyvault::SecretRef>, String> {
    let config = config_from()?;
    let transport = ReqwestSecretTransport::new(&config).map_err(|error| error.to_string())?;
    KeyvaultClient::new(transport)
        .list()
        .await
        .map_err(|error| error.to_string())
}

/// Fetches one secret, opens it, and hands the plaintext to `write` — which
/// the clipboard path supplies.
///
/// The write is a parameter so a test can be *in* it: arming the suppression
/// before the write is this module's central promise, and the only assertion
/// that proves the ordering observes the deadline from inside the write.
pub async fn copy_secret_with<R: Runtime, T: SecretTransport>(
    app: &AppHandle<R>,
    transport: T,
    config: &KeyvaultConfig,
    slug: &str,
    write: impl Fn(&str) -> Result<(), String>,
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
    write(plaintext.as_str())
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
    copy_secret_with(app, transport, config, slug, |text| {
        // Asked through try_state rather than `clipboard()`: the extension
        // method panics when the plugin is absent, and "the clipboard is
        // unavailable" is the honest answer a panic would swallow.
        match app.try_state::<tauri_plugin_clipboard_manager::Clipboard<R>>() {
            Some(clipboard) => clipboard
                .write_text(text)
                .map_err(|_| "clipboard_unavailable".to_owned()),
            None => Err("clipboard_unavailable".to_owned()),
        }
    })
    .await
}

// ------------------------------------------------------------------ pairing --

/// A pairing waiting on the browser.
///
/// Held in memory only. If the application closes mid-pairing the session is abandoned rather
/// than resumed, which is the honest outcome: the code is short-lived, and a half-finished
/// pairing restored from disk would be a credential nobody remembers granting.
struct PendingPairing {
    base_url: String,
    code: String,
    private_jwk: String,
}

fn pending() -> &'static Mutex<Option<PendingPairing>> {
    static PENDING: OnceLock<Mutex<Option<PendingPairing>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}

/// What a started pairing gives the interface to show.
///
/// The link and the code are here because the browser that opened may not be the one someone is
/// signed into — `/usr/bin/open` uses the default, and a session lives in one browser's storage.
/// Without something to copy, a pairing started in the wrong browser is a dead end.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStartedDto {
    pub fingerprint: String,
    pub url: String,
    pub code: String,
    pub expires_at: i64,
}

/// Where a pairing stands, in the words the interface shows.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatusDto {
    pub status: &'static str,
}

impl PairingStatusDto {
    fn of(status: &'static str) -> Self {
        Self { status }
    }
}

/// Begins a pairing: mints this device a keypair, opens one, and sends the browser to approve it.
///
/// Returns the fingerprint, which the interface must show. It is the only thing tying the page
/// someone is about to approve to the application that asked — without comparing it, approving
/// means trusting whatever code happens to be in the address bar.
pub async fn pair_start_service(base_url: String) -> Result<PairingStartedDto, String> {
    // The typed address overrides; it is not a requirement. Pairing clears that field on success,
    // so demanding it made the second pairing impossible: the act of pairing removed the only
    // thing the next one could read. The device already knows where its vault is.
    let typed = base_url.trim().to_owned();
    let base_url = if typed.is_empty() {
        clipboard_keyvault::known_base_url(clipboard_keyvault::CONSUMER)
            .ok_or_else(|| clipboard_keyvault::KeyvaultError::NoVaultAddress.to_string())?
    } else {
        // Whatever was typed, resolved to the API. The address someone knows is the one they
        // visit, and that is a different host from the API — asking them to know the second is
        // asking them to know a deployment detail.
        clipboard_keyvault::resolve_api(&typed)
            .await
            .map_err(|error| error.to_string())?
    };
    clipboard_keyvault::validate_base_url(&base_url).map_err(|error| error.to_string())?;

    // Asked before anything is generated: a deployment with no published interface cannot be
    // paired at all, and spending two seconds on a keypair first would only delay saying so.
    let page = clipboard_keyvault::pairing_page_url(&base_url)
        .await
        .map_err(|error| error.to_string())?;

    // Generating 3072-bit RSA takes a second or two of solid arithmetic; off the async runtime so
    // it cannot stall everything else the application is doing.
    let key = tokio::task::spawn_blocking(clipboard_keyvault::generate_device_key)
        .await
        .map_err(|_| "keyvault_pairing_failed".to_owned())?
        .map_err(|error| error.to_string())?;

    let started =
        clipboard_keyvault::start_pairing(&base_url, "Clipboard History", &key.public_jwk)
            .await
            .map_err(|error| error.to_string())?;

    let url = format!("{page}/pair?code={}", urlencoding_minimal(&started.code));
    {
        let mut slot = pending()
            .lock()
            .expect("the pairing slot is only held briefly");
        *slot = Some(PendingPairing {
            base_url,
            code: started.code.clone(),
            private_jwk: key.private_jwk,
        });
    }
    // Opened as a convenience, and its failure is not the end of the pairing: the interface shows
    // the link, which is the way through when the default browser is the wrong one.
    let _ = open_in_browser(&url);
    Ok(PairingStartedDto {
        fingerprint: started.fingerprint,
        url,
        code: started.code,
        expires_at: started.expires_at,
    })
}

/// Asks once whether the browser has approved, and records the result if it has.
///
/// The interface calls this on a timer, so every answer other than "pending" also clears the
/// slot: a pairing that expired or was collected by someone else must not be polled forever.
pub async fn pair_poll_service(state: &AppState) -> Result<PairingStatusDto, String> {
    // The lock is released before the request. Holding it across an await would block the
    // interface's next call on the network, and a cancel would have nothing to take.
    let Some((base_url, code, private_jwk)) = ({
        let slot = pending()
            .lock()
            .expect("the pairing slot is only held briefly");
        slot.as_ref()
            .map(|p| (p.base_url.clone(), p.code.clone(), p.private_jwk.clone()))
    }) else {
        return Ok(PairingStatusDto::of("idle"));
    };

    let outcome = clipboard_keyvault::claim_pairing(&base_url, &code, &private_jwk)
        .await
        .map_err(|error| error.to_string())?;

    match outcome {
        clipboard_keyvault::PairingOutcome::Pending => Ok(PairingStatusDto::of("pending")),
        clipboard_keyvault::PairingOutcome::Approved { url, token } => {
            clipboard_keyvault::save_paired(
                clipboard_keyvault::CONSUMER,
                &url,
                &private_jwk,
                &token,
            )
            .map_err(|error| error.to_string())?;
            // Only after the identity is safely on disk: clearing first would leave an install
            // with neither the old configuration nor the new one if the write failed.
            clear_settings_overrides(state).await?;
            pair_cancel_service();
            Ok(PairingStatusDto::of("paired"))
        }
        clipboard_keyvault::PairingOutcome::Expired => {
            pair_cancel_service();
            Ok(PairingStatusDto::of("expired"))
        }
        clipboard_keyvault::PairingOutcome::NotFound => {
            pair_cancel_service();
            Ok(PairingStatusDto::of("notFound"))
        }
        clipboard_keyvault::PairingOutcome::AlreadyClaimed => {
            pair_cancel_service();
            Ok(PairingStatusDto::of("alreadyClaimed"))
        }
    }
}

/// Drops the URL and token overrides once a pairing has replaced them.
///
/// The token and key a previous version could store here are gone from the read path entirely,
/// but a row written by one still carries them. Clearing on a pairing is what finally removes
/// them from an install that has been through those versions.
pub async fn clear_settings_overrides(state: &AppState) -> Result<(), String> {
    let store = state.store.clone();
    let settings = crate::commands::run_blocking("settings_unavailable", move || {
        crate::commands::get_settings_blocking(&store)
    })
    .await?;

    // Nothing to clear is the common case, and writing the row anyway would touch settings on
    // every pairing for no reason.
    if settings.keyvault.url.is_none() && settings.keyvault.token.is_none() {
        return Ok(());
    }

    let mut cleared = settings;
    cleared.keyvault.url = None;
    cleared.keyvault.token = None;
    // Through the ordinary save so there is one way into this row, with its validation and the
    // side effects that hang off it.
    crate::commands::save_settings_service(state, cleared).await?;
    Ok(())
}

/// Forgets the pairing in flight, and with it this device's unsaved key./// Forgets the pairing in flight, and with it this device's unsaved key.
pub fn pair_cancel_service() {
    let mut slot = pending()
        .lock()
        .expect("the pairing slot is only held briefly");
    *slot = None;
}

/// Percent-encodes what a pairing code may contain.
///
/// Codes are base64url, so only `-` and `_` beyond alphanumerics, and none of those need encoding.
/// Anything else would mean the vault changed its alphabet, and passing it through unescaped is
/// how a query string quietly becomes two.
fn urlencoding_minimal(code: &str) -> String {
    code.chars()
        .flat_map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                vec![character]
            } else {
                format!("%{:02X}", character as u32 as u8).chars().collect()
            }
        })
        .collect()
}

/// Hands the URL to the browser. Passed as one argument and never through a shell.
fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg(url)
            .status()
            .map_err(|_| "keyvault_browser_failed".to_owned())
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err("keyvault_browser_failed".to_owned())
                }
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("keyvault_browser_failed".to_owned())
    }
}

/// What the settings pane needs to show about this device's pairing.
///
/// A file read, not a request: the pane asks on every open and the answer is on disk. Whether a
/// vault can actually be reached is what Test connection is for.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityDto {
    pub paired: bool,
    /// The vault this device knows, paired or not. It is what the address field displays, and
    /// what a Reset leaves behind so pairing again is one press rather than a retyping.
    pub url: Option<String>,
}

/// Reports the pairing state.
///
/// The identity is the truth here, not the settings row — the row has been wrong before, and
/// showing a value nobody is using is how the last round of confusion started.
pub fn identity_service() -> IdentityDto {
    IdentityDto {
        paired: clipboard_keyvault::load_device_identity(clipboard_keyvault::CONSUMER).is_ok(),
        url: clipboard_keyvault::known_base_url(clipboard_keyvault::CONSUMER),
    }
}

/// Forgets this device's pairing so it can pair again.
///
/// Local only. The device registered in the vault keeps existing: revoking it is an authenticated
/// mutation, and this application holds a token rather than an account. The next pairing offers to
/// retire it and the vault's Devices screen can do it by hand — which the interface says, because
/// a "Reset" that quietly leaves a working credential behind would be a lie.
pub async fn reset_pairing_service(state: &AppState) -> Result<(), String> {
    clipboard_keyvault::forget_paired(clipboard_keyvault::CONSUMER)
        .map_err(|error| error.to_string())?;
    // The row's vestigial fields go too, so nothing is left to reappear in the pane.
    clear_settings_overrides(state).await
}
