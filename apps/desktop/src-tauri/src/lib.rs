pub mod analyzer;
pub mod command_error;
pub mod commands;
pub mod events;
pub mod git_status;
pub mod java;
pub mod knowledge_graph;
pub mod kotlin;
pub mod persistence;
pub mod project_detection;
pub mod scanner;
pub mod spring;
pub mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
        .setup(|app| {
            use tauri::Manager;

            let data_dir = app.path().app_data_dir().map_err(|error| error.to_string());
            app.manage(persistence::PersistenceState::initialize(data_dir));
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_info,
            commands::git::get_git_status,
            commands::project::open_project,
            commands::project::get_last_project,
            commands::graph::save_graph_snapshot,
            commands::graph::list_graph_snapshots,
            commands::graph::get_graph_snapshot,
            commands::spring::analyze_spring_project,
            commands::scanner::scan_project,
            commands::tasks::cancel_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
