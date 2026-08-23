pub mod commands;
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
        .setup(|app| {
            let data_dir = state::resolve_data_dir(app.handle())?;
            let app_state = state::AppState::open_data_dir(data_dir)?;
            app.manage(app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_history,
            commands::get_preview,
            commands::set_pinned,
            commands::delete_event,
            commands::copy_event,
            commands::analyze_import,
            commands::start_import,
            commands::discard_import_analysis,
            commands::get_import_status,
            commands::get_settings,
            commands::save_settings,
            commands::get_storage_stats,
            commands::get_thumbnail,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Clipboard History");
}
