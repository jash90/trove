pub mod chat;
pub mod commands;
pub mod export;
pub mod hotkey;
pub mod keyvault;
pub mod links;
pub mod maintenance;
pub mod monitor;
pub mod snap;
pub mod state;
pub mod tray;

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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let data_dir = state::resolve_data_dir(app.handle())?;
            let app_state = state::AppState::open_data_dir(data_dir)?;
            // The menu bar is where this application exists on screen, so it
            // starts with no Dock tile unless the settings row asks for one.
            // `LSUIElement` in Info.plist is what stops the tile appearing
            // during launch, before any of this has run; this call is what
            // brings it back for someone who wants it, and the only thing
            // that applies the preference at all under `pnpm tauri dev`,
            // where there is no bundle to read the plist from.
            #[cfg(target_os = "macos")]
            let _ = app
                .handle()
                .set_dock_visibility(commands::dock_icon_enabled(&app_state.store));
            let purge_store = app_state.store.clone();
            tauri::async_runtime::spawn(async move {
                purge_retired_settings(&purge_store).await;
            });
            // The shortcut the user chose, not the built-in one. Startup used
            // to register the default unconditionally, so a shortcut changed in
            // settings answered until the application was closed and then
            // reverted without saying so.
            let shortcut =
                hotkey::shortcut_for_launch(commands::stored_hotkey(&app_state.store).as_deref());
            // The snap shortcuts ride along with the summoning one: read from
            // the same settings row, registered the same way, remembered so a
            // save knows what to take down. Read before the state is managed,
            // which is where the store moves.
            let snap_map = commands::snap_shortcuts(&app_state.store);
            app.manage(app_state);
            let active = hotkey::ActiveShortcut::new(shortcut);
            // A shortcut another application already holds is a degraded state,
            // not a reason to refuse to start: the palette still opens from the
            // menu bar. What it is not is a state to keep quiet about, so the
            // answer is recorded where the settings screen can read it.
            match hotkey::install(app.handle(), shortcut) {
                Ok(()) => active.set_registered(true),
                Err(_) => eprintln!("trove: global shortcut unavailable"),
            }
            app.manage(active);
            let active_snap = snap::ActiveSnapShortcuts::new(snap_map.clone());
            snap::install(app.handle(), &snap_map);
            app.manage(active_snap);
            app.manage(hotkey::ReleasedSystemHotkeys::new());
            app.manage(hotkey::PasteTarget::new());
            app.manage(hotkey::PastePrompt::new());
            let control = monitor::MonitorControl::new();
            app.manage(control.clone());
            // The window spends most of its life hidden, so the menu bar is
            // where the application exists on screen. Failing to place it there
            // is not a reason to refuse to start.
            if let Err(error) = tray::install(app.handle(), control.clone()) {
                eprintln!("trove: menu bar item unavailable ({error})");
            }
            monitor::start(app.handle(), control);
            maintenance::start(app.handle());
            // A window that never becomes key is never composited: launched
            // without being activated, the palette stayed a blank rectangle
            // until something brought it forward. Asking for focus once at
            // startup is what a manually launched application does anyway.
            hotkey::show_palette(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                hotkey::hide_instead_of_closing(window, api);
            }
            // The palette entered by a click — not by its shortcut — still
            // owes its snaps the window the user was last working in, so
            // coming forward is when the target is remembered.
            if let tauri::WindowEvent::Focused(focused) = event {
                if *focused && window.label() == "main" {
                    hotkey::remember_target_on_focus(window.app_handle());
                }
            }
        })
        .invoke_handler(commands::invoke_handler())
        .build(tauri::generate_context!())
        .expect("error while building Trove")
        .run(|app, event| {
            // Clicking the Dock tile has to summon something, or the tile is
            // a button that does nothing. macOS sends this when the tile is
            // clicked with no window on screen, and the palette is what the
            // click is asking for.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                hotkey::show_palette(app);
            }
            // Neither parameter is read off macOS, and the workspace builds
            // with `-D warnings`.
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}
