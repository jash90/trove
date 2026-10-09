//! The parts of launching that decide how loudly to launch, and what to say
//! when launching is impossible.

use std::path::Path;

use trove_store::StoreError;

/// The argument the login item passes, so a launch at login can be told apart
/// from one the person asked for.
pub const AUTOSTART_ARG: &str = "--autostart";

/// Whether this process was started by the login item.
///
/// A launch at login is the system starting a menu bar application in the
/// background; nobody is waiting for a window, and a palette appearing over
/// whatever the person opened first is an interruption. Every other launch —
/// Finder, Spotlight, `open` — is someone asking for the palette, and it is
/// shown.
pub fn launched_at_login<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    // The first argument is the executable's own path, which can be anything;
    // only what follows it was passed to the application.
    args.into_iter()
        .skip(1)
        .any(|arg| arg.as_ref() == AUTOSTART_ARG)
}

/// Why the history could not be opened, in the terms the person can act on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupFailure {
    /// The database was written by a newer build, or by one this build does
    /// not recognise. Opening it anyway could corrupt it; installing the
    /// newer build is the fix.
    SchemaNewer,
    /// The data folder is not private to this user, or cannot be written.
    Permissions,
    /// Another process holds the database — usually a second copy of Trove.
    Locked,
    /// Anything else: the storage itself is not usable as it stands.
    Unavailable,
}

impl StartupFailure {
    /// Reads the cause out of whatever `open_data_dir` or `resolve_data_dir`
    /// returned, looking through the context `anyhow` wraps it in.
    pub fn classify(error: &anyhow::Error) -> Self {
        for cause in error.chain() {
            if let Some(store) = cause.downcast_ref::<StoreError>() {
                return Self::from_store(store);
            }
            if let Some(io) = cause.downcast_ref::<std::io::Error>()
                && io.kind() == std::io::ErrorKind::PermissionDenied
            {
                return Self::Permissions;
            }
        }
        Self::Unavailable
    }

    fn from_store(error: &StoreError) -> Self {
        match error {
            StoreError::UnsupportedSchemaVersion(_) | StoreError::IncompatibleSchema => {
                Self::SchemaNewer
            }
            StoreError::PrivateStorageUnavailable => Self::Permissions,
            StoreError::Database(database) => match database.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    Self::Locked
                }
                Some(
                    rusqlite::ErrorCode::PermissionDenied
                    | rusqlite::ErrorCode::ReadOnly
                    | rusqlite::ErrorCode::CannotOpen,
                ) => Self::Permissions,
                _ => Self::Unavailable,
            },
            _ => Self::Unavailable,
        }
    }

    /// The alert's headline and the sentence under it.
    pub fn message(self) -> (&'static str, &'static str) {
        match self {
            Self::SchemaNewer => (
                "Trove cannot open your history",
                "It was saved by a newer version of Trove. Install the latest \
                 version to open it; this one has left it untouched.",
            ),
            Self::Permissions => (
                "Trove cannot use its data folder",
                "The folder must belong to you and be readable only by you. \
                 Check its owner and permissions, then open Trove again.",
            ),
            Self::Locked => (
                "Trove's history is in use",
                "Another copy of Trove, or another program, has the history \
                 open. Quit it and open Trove again.",
            ),
            Self::Unavailable => (
                "Trove cannot open its storage",
                "The history database could not be opened. The details are in \
                 ~/Library/Logs/Trove/trove.log.",
            ),
        }
    }
}

/// Logs why the launch stopped, tells the person, and ends the process.
///
/// Returning an error from `setup` would have Tauri panic with "Failed to
/// setup app", which for an application with no Dock tile and no window yet
/// means nothing at all happens on screen. This is the visible version of
/// the same outcome.
pub fn refuse_to_start(error: &anyhow::Error, data_dir: Option<&Path>) -> ! {
    let failure = StartupFailure::classify(error);
    crate::log::log_line(&format!(
        "cannot start ({failure:?}): {error:#}{}",
        data_dir
            .map(|dir| format!(" [data folder: {}]", dir.display()))
            .unwrap_or_default()
    ));
    #[cfg(target_os = "macos")]
    {
        let (message, detail) = failure.message();
        let choice = platform_macos::alert::show_startup_failure(
            message,
            detail,
            data_dir.is_some_and(Path::exists),
        );
        if choice == platform_macos::StartupAlertChoice::ShowDataFolder
            && let Some(dir) = data_dir
        {
            platform_macos::alert::reveal_in_finder(dir);
        }
    }
    std::process::exit(1);
}

/// Rewrites the login item so it carries [`AUTOSTART_ARG`].
///
/// Login items registered before the argument existed launch the application
/// bare, which reads as a manual launch and shows the palette at every login.
/// Enabling again overwrites the item in place, with the argument; it is
/// deliberately not a disable followed by an enable, which would leave the
/// person with no login item at all if the second half failed.
///
/// Only from an application bundle: a development binary rewriting the item
/// would point the next login at a build directory.
pub fn refresh_login_item<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let in_bundle = std::env::current_exe()
        .map(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false);
    if !in_bundle {
        return;
    }
    let app = app.clone();
    // Off the main thread: it is file I/O, and the main thread is what draws
    // the palette this launch may be about to show.
    std::thread::spawn(move || {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        match launcher.is_enabled() {
            Ok(true) => {
                if let Err(error) = launcher.enable() {
                    crate::log::log_line(&format!("login item not refreshed: {error}"));
                }
            }
            Ok(false) => {}
            Err(error) => {
                crate::log::log_line(&format!("login item state unreadable: {error}"));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_at_login_is_told_apart_from_one_someone_asked_for() {
        assert!(launched_at_login([
            "/Applications/Trove.app/Contents/MacOS/trove-app",
            "--autostart"
        ]));
        assert!(!launched_at_login([
            "/Applications/Trove.app/Contents/MacOS/trove-app"
        ]));
        // Finder can append its own arguments to a manual launch; they are
        // not the login item's.
        assert!(!launched_at_login([
            "/Applications/Trove.app/Contents/MacOS/trove-app",
            "-psn_0_12345"
        ]));
        // The executable's own path is not an argument, whatever it is called.
        assert!(!launched_at_login(["--autostart"]));
    }

    #[test]
    fn a_newer_schema_is_reported_as_a_version_problem() {
        let error = anyhow::Error::new(StoreError::UnsupportedSchemaVersion(99));
        assert_eq!(
            StartupFailure::classify(&error),
            StartupFailure::SchemaNewer
        );
        let error = anyhow::Error::new(StoreError::IncompatibleSchema);
        assert_eq!(
            StartupFailure::classify(&error),
            StartupFailure::SchemaNewer
        );
    }

    #[test]
    fn a_folder_that_is_not_private_is_reported_as_permissions() {
        let error = anyhow::Error::new(StoreError::PrivateStorageUnavailable)
            .context("opening the data folder");
        assert_eq!(
            StartupFailure::classify(&error),
            StartupFailure::Permissions
        );
        let error = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(
            StartupFailure::classify(&error),
            StartupFailure::Permissions
        );
    }

    #[test]
    fn a_busy_database_is_reported_as_in_use() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        let error = anyhow::Error::new(StoreError::Database(busy));
        assert_eq!(StartupFailure::classify(&error), StartupFailure::Locked);
    }

    #[test]
    fn anything_else_is_reported_as_unavailable() {
        let error = anyhow::Error::new(StoreError::WalUnavailable("delete".to_owned()));
        assert_eq!(
            StartupFailure::classify(&error),
            StartupFailure::Unavailable
        );
        assert_eq!(
            StartupFailure::classify(&anyhow::anyhow!("no home directory")),
            StartupFailure::Unavailable
        );
    }
}
