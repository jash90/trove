pub mod commands;
pub mod export;
pub mod hotkey;
pub mod keyvault;
pub mod links;
pub mod maintenance;
pub mod monitor;
pub mod state;
pub mod tray;

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
            app.manage(app_state);
            // A shortcut another application already holds is a degraded
            // state, not a reason to refuse to start: the palette still opens
            // from its own window.
            if hotkey::install(app.handle()).is_err() {
                eprintln!("clipboard-history: global shortcut unavailable");
            }
            app.manage(hotkey::PasteTarget::new());
            app.manage(hotkey::ActiveShortcut::default());
            let control = monitor::MonitorControl::new();
            app.manage(control.clone());
            // The window spends most of its life hidden, so the menu bar is
            // where the application exists on screen. Failing to place it there
            // is not a reason to refuse to start.
            if let Err(error) = tray::install(app.handle(), control.clone()) {
                eprintln!("clipboard-history: menu bar item unavailable ({error})");
            }
            monitor::start(app.handle(), control);
            maintenance::start(app.handle());
            // A window that never becomes key is never composited: launched
            // without being activated, the palette stayed a blank rectangle
            // until something brought it forward. Asking for focus once at
            // startup is what a manually launched application does anyway.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                hotkey::hide_instead_of_closing(window, api);
            }
        })
        .invoke_handler(commands::invoke_handler())
        .run(tauri::generate_context!())
        .expect("error while running Clipboard History");
}
