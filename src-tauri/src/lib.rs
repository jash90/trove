pub mod commands;
pub mod hotkey;
pub mod monitor;
pub mod state;

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
            let control = monitor::MonitorControl::new();
            app.manage(control.clone());
            monitor::start(app.handle(), control);
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
