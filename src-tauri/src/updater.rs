//! Checking for, and installing, a newer release.
//!
//! Nothing here runs by itself: the application goes online for an update only
//! when someone presses the button in settings or picks the menu bar entry that
//! leads there. That keeps the promise the README makes about network calls.
//!
//! The work is done by `tauri-plugin-updater`. It reads `latest.json` from the
//! public releases repository, refuses an archive whose minisign signature does
//! not match the key compiled into the application, and swaps the `.app` bundle
//! in place. What lives here is the part the interface needs: a check that
//! remembers what it found, so the install that follows cannot be pointed at a
//! different release, and error codes the settings screen can put into words.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, ipc::Channel};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};

/// The release the last check found, waiting for the user to accept it.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

impl PendingUpdate {
    fn replace(&self, update: Option<Update>) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = update;
    }

    /// Takes the pending release, so a second press of the install button
    /// cannot start a second download of it.
    fn take(&self) -> Result<Update, String> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| "no_pending_update".to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfoDto {
    pub current_version: String,
    pub available: bool,
    pub version: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgressDto {
    pub downloaded: u64,
    /// Absent when the server sent no length, which GitHub always does send.
    pub total: Option<u64>,
}

/// Puts a plugin failure into one of the few sentences the settings screen has.
///
/// The plugin's own messages are written for developers — "the platform
/// `darwin-x86_64` was not found in the response `platforms` object" — and
/// would reach the user verbatim otherwise.
pub fn error_code(error: &UpdaterError) -> &'static str {
    match error {
        UpdaterError::Reqwest(_)
        | UpdaterError::Network(_)
        | UpdaterError::ReleaseNotFound
        | UpdaterError::Http(_)
        | UpdaterError::Serialization(_) => "update_unreachable",
        UpdaterError::TargetNotFound(_)
        | UpdaterError::TargetsNotFound(_)
        | UpdaterError::UnsupportedArch
        | UpdaterError::UnsupportedOs => "update_no_build",
        UpdaterError::Minisign(_)
        | UpdaterError::Base64(_)
        | UpdaterError::SignatureUtf8(_)
        | UpdaterError::SignedVersionMismatch { .. }
        | UpdaterError::MissingSignedVersion => "update_signature_invalid",
        UpdaterError::AuthenticationFailed => "update_not_writable",
        UpdaterError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
            "update_not_writable"
        }
        _ => "update_failed",
    }
}

fn info_for(current_version: String, update: Option<&Update>) -> UpdateInfoDto {
    UpdateInfoDto {
        current_version,
        available: update.is_some(),
        version: update.map(|update| update.version.clone()),
        notes: update
            .and_then(|update| update.body.clone())
            .filter(|notes| !notes.trim().is_empty()),
    }
}

/// Asks the releases repository whether something newer than this build exists.
#[tauri::command(rename_all = "camelCase")]
pub async fn check_for_update<R: Runtime>(app: AppHandle<R>) -> Result<UpdateInfoDto, String> {
    let current_version = app.package_info().version.to_string();
    let updater = app
        .updater()
        .map_err(|error| error_code(&error).to_owned())?;
    let update = updater
        .check()
        .await
        .map_err(|error| error_code(&error).to_owned())?;
    let info = info_for(current_version, update.as_ref());
    app.state::<PendingUpdate>().replace(update);
    Ok(info)
}

/// Downloads the release the last check found, installs it, and restarts.
///
/// Progress goes over a channel rather than events: it belongs to this one
/// call, and nothing else in the application should be able to hear it.
#[tauri::command(rename_all = "camelCase")]
pub async fn install_update<R: Runtime>(
    app: AppHandle<R>,
    on_progress: Channel<UpdateProgressDto>,
) -> Result<(), String> {
    let update = app.state::<PendingUpdate>().take()?;
    let mut downloaded = 0_u64;
    update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = on_progress.send(UpdateProgressDto { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|error| error_code(&error).to_owned())?;
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_without_a_check_is_refused_rather_than_guessed_at() {
        let pending = PendingUpdate::default();
        assert_eq!(pending.take().err().as_deref(), Some("no_pending_update"));
    }

    #[test]
    fn plugin_failures_become_codes_the_settings_screen_can_word() {
        assert_eq!(
            error_code(&UpdaterError::ReleaseNotFound),
            "update_unreachable"
        );
        assert_eq!(
            error_code(&UpdaterError::Network("timed out".into())),
            "update_unreachable"
        );
        // An Intel Mac asking a release that only ships aarch64.
        assert_eq!(
            error_code(&UpdaterError::TargetNotFound("darwin-x86_64".into())),
            "update_no_build"
        );
        assert_eq!(
            error_code(&UpdaterError::SignatureUtf8("garbage".into())),
            "update_signature_invalid"
        );
        assert_eq!(
            error_code(&UpdaterError::MissingSignedVersion),
            "update_signature_invalid"
        );
        assert_eq!(
            error_code(&UpdaterError::AuthenticationFailed),
            "update_not_writable"
        );
        assert_eq!(
            error_code(&UpdaterError::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            ))),
            "update_not_writable"
        );
        assert_eq!(
            error_code(&UpdaterError::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound
            ))),
            "update_failed"
        );
        assert_eq!(error_code(&UpdaterError::EmptyEndpoints), "update_failed");
    }

    #[test]
    fn up_to_date_reports_only_the_running_version() {
        assert_eq!(
            info_for("1.8.1".into(), None),
            UpdateInfoDto {
                current_version: "1.8.1".into(),
                available: false,
                version: None,
                notes: None,
            }
        );
    }
}
