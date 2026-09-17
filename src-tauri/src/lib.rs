pub mod chat;
pub mod commands;
pub mod export;
pub mod hotkey;
pub mod keyvault;
pub mod links;
pub mod maintenance;
pub mod monitor;
pub mod state;
pub mod tray;
pub mod typesafe;

use tauri::Manager;

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
            // The menu bar is where this application exists on screen. A Dock
            // icon promises a window that spends its life hidden, and clicking
            // it does nothing worth doing. `LSUIElement` in Info.plist keeps
            // the tile from ever appearing in a built bundle; this covers
            // `pnpm tauri dev`, where there is no bundle to read it from.
            #[cfg(target_os = "macos")]
            let _ = app.handle().set_dock_visibility(false);
            // The shortcut the user chose, not the built-in one. Startup used
            // to register the default unconditionally, so a shortcut changed in
            // settings answered until the application was closed and then
            // reverted without saying so.
            let shortcut =
                hotkey::shortcut_for_launch(commands::stored_hotkey(&app_state.store).as_deref());
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
        })
        .invoke_handler(commands::invoke_handler())
        .run(tauri::generate_context!())
        .expect("error while running Trove");
}
