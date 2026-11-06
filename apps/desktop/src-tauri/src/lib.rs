pub mod command_error;
pub mod commands;
pub mod events;
pub mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_info,
            commands::project::open_project,
            commands::tasks::cancel_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
