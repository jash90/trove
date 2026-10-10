pub mod chat;
pub mod commands;
pub mod export;
pub mod hotkey;
pub mod keyvault;
pub mod links;
pub mod locale;
pub mod log;
pub mod maintenance;
pub mod monitor;
pub mod snap;
pub mod startup;
pub mod state;
pub mod tray;
pub mod updater;

use tauri::Manager;

/// Clears settings rows whose feature has been removed.
///
/// The privacy scan is gone, and the TypeSafe API key it kept must not
/// outlive it: it is a live credential of the user's, sitting in a row that
/// nothing reads any more. This runs at every launch and costs one DELETE
/// against a key that is usually absent — cheap enough not to need a flag
/// tracking whether it has run, and a flag would be one more thing that can
/// be wrong.
pub async fn purge_retired_settings(store: &trove_store::StoreHandle) {
    // A failure here is not worth refusing to start over: the next launch
    // tries again, and nothing downstream depends on the row being gone.
    let _ = store.delete_setting("typesafe").await;
}

/// Whether this process turns away a second launch through the
/// single-instance plugin.
///
/// Not when `TROVE_DATA_DIR` is set: that is how a development or test run is
/// pointed at a history of its own, and it has to be able to start alongside
/// the installed application, which the plugin would refuse because it keys
/// on the bundle identifier the two share. Such a run is still guarded by the
/// instance lock kept beside its own data directory.
pub fn single_instance_enabled() -> bool {
    std::env::var_os("TROVE_DATA_DIR").is_none()
}

pub fn run() {
    let mut builder = tauri::Builder::default();
    // First, so a second launch exits before any other plugin has done
    // anything on its behalf. Two instances record every copy twice, put two
    // items on the menu bar, and let one reclaim blobs the other has written
    // but not yet committed. A second launch can come from the login item,
    // which runs the binary directly rather than through Launch Services,
    // from `open -n`, from a second copy of the bundle, or from an update's
    // restart. The running instance is told instead, and answers the way a
    // click on its Dock tile is answered: with the palette.
    if single_instance_enabled() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A second launch by the login item — the user already had Trove
            // open when they logged in — is as quiet as a first one.
            if startup::launched_at_login(&argv) {
                return;
            }
            // The plugin calls this from a task on the async runtime, and
            // summoning the palette reads the frontmost application and moves
            // a window, both of which belong on the main thread.
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || hotkey::show_palette(&handle));
        }));
    }
    builder
        .plugin(tauri_plugin_dialog::init())
        // The login item passes an argument so the launch it causes can stay
        // quiet: at login the menu bar item is all anyone expects to appear.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![startup::AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Before the data directory is resolved, because resolving it can
            // move the legacy directory, and that is already writing to the
            // history. No step here may use `?`: an error returned from setup
            // becomes a panic nobody sees.
            match state::instance_lock_path(app.handle()) {
                Ok(lock_path) => {
                    match state::InstanceLock::acquire(&lock_path, state::INSTANCE_LOCK_WAIT) {
                        Ok(lock) => {
                            app.manage(lock);
                        }
                        // Another instance is not a failure worth an alert:
                        // the one already running is the answer.
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            log::log_line("another instance is already running");
                            std::process::exit(0);
                        }
                        // A lock file that cannot be opened is not another
                        // instance, and refusing to start over it would take
                        // the history away over a guard. The single-instance
                        // plugin still stands.
                        Err(error) => {
                            log::log_line(&format!("instance lock unavailable ({error})"))
                        }
                    }
                }
                // Likewise for a lock path that cannot be worked out: if the
                // data directory itself is the problem, opening it below
                // fails too, and that failure is the one explained.
                Err(error) => log::log_line(&format!("instance lock unavailable ({error:#})")),
            }
            // A data folder that cannot be opened ends the launch, but not as
            // an error returned from here: Tauri turns that into a panic, and
            // with no Dock tile and no window yet the person would see nothing
            // happen at all. `refuse_to_start` logs, explains and exits.
            let data_dir = match state::resolve_data_dir(app.handle()) {
                Ok(dir) => dir,
                Err(error) => startup::refuse_to_start(&error, None),
            };
            let app_state = match state::AppState::open_data_dir(&data_dir) {
                Ok(app_state) => app_state,
                Err(error) => startup::refuse_to_start(&error, Some(&data_dir)),
            };
            // Read once, for everything below that depends on it.
            let settings = commands::launch_settings(&app_state.store);
            // The menu bar is where this application exists on screen, so it
            // starts with no Dock tile unless the settings row asks for one.
            // `LSUIElement` in Info.plist is what stops the tile appearing
            // during launch, before any of this has run; this call is what
            // brings it back for someone who wants it, and the only thing
            // that applies the preference at all under `pnpm tauri dev`,
            // where there is no bundle to read the plist from.
            #[cfg(target_os = "macos")]
            let _ = app.handle().set_dock_visibility(settings.dock_icon);
            let purge_store = app_state.store.clone();
            tauri::async_runtime::spawn(async move {
                purge_retired_settings(&purge_store).await;
            });
            // The shortcut the user chose, not the built-in one. Startup used
            // to register the default unconditionally, so a shortcut changed in
            // settings answered until the application was closed and then
            // reverted without saying so.
            let shortcut = hotkey::shortcut_for_launch(Some(settings.hotkey.as_str()));
            // The snap shortcuts ride along with the summoning one: read from
            // the same settings row, registered the same way, remembered so a
            // save knows what to take down.
            let snap_map = settings.snap_shortcuts;
            app.manage(app_state);
            let active = hotkey::ActiveShortcut::new(shortcut);
            // A shortcut another application already holds is a degraded state,
            // not a reason to refuse to start: the palette still opens from the
            // menu bar. What it is not is a state to keep quiet about, so the
            // answer is recorded where the settings screen can read it.
            match hotkey::install(app.handle(), shortcut) {
                Ok(()) => active.set_registered(true),
                Err(error) => log::log_line(&format!("global shortcut unavailable ({error})")),
            }
            app.manage(active);
            let active_snap = snap::ActiveSnapShortcuts::new(snap_map.clone());
            snap::install(app.handle(), &snap_map);
            app.manage(active_snap);
            app.manage(hotkey::ReleasedSystemHotkeys::new());
            app.manage(hotkey::PasteTarget::new());
            app.manage(hotkey::PastePrompt::new());
            app.manage(updater::PendingUpdate::default());
            let control = monitor::MonitorControl::new();
            app.manage(control.clone());
            // The window spends most of its life hidden, so the menu bar is
            // where the application exists on screen. Failing to place it there
            // is not a reason to refuse to start.
            // The window titles in tauri.conf.json are the English ones; the
            // shell's language replaces them before any window is shown.
            for (label, title) in [
                ("settings", locale::Text::SettingsWindowTitle),
                ("chat", locale::Text::ChatWindowTitle),
            ] {
                if let Some(window) = app.get_webview_window(label) {
                    let _ = window.set_title(locale::tr(title));
                }
            }
            if let Err(error) = tray::install(app.handle(), control.clone()) {
                log::log_line(&format!("menu bar item unavailable ({error})"));
            }
            monitor::start(app.handle(), control);
            maintenance::start(app.handle());
            // Login items written before the quiet-launch argument existed
            // would go on showing the palette at every login; this brings
            // them up to date in the background.
            startup::refresh_login_item(app.handle());
            // The palette starts hidden (`visible: false`), so nothing sits
            // on screen as a blank rectangle while the store opens. A manual
            // launch then shows it: a window that never becomes key is never
            // composited, and asking for focus once is what a manually
            // launched application does anyway. A launch at login does not —
            // the menu bar item is the whole of its arrival.
            if !startup::launched_at_login(std::env::args()) {
                hotkey::show_palette(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                hotkey::hide_instead_of_closing(window, api);
            }
            if let tauri::WindowEvent::Focused(focused) = event {
                hotkey::window_focus_changed(window, *focused);
            }
        })
        .invoke_handler(commands::invoke_handler())
        .build(tauri::generate_context!())
        .expect("error while building Trove")
        .run(|app, event| {
            // Clicking the Dock tile has to summon something, or the tile is
            // a button that does nothing. Which window it brings back is
            // `hotkey::reopen`'s to decide.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                hotkey::reopen(app);
            }
            // Neither parameter is read off macOS, and the workspace builds
            // with `-D warnings`.
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}
