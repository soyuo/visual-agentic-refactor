pub mod ai_provider;
pub mod analyzer;
pub mod anthropic_provider;
pub mod command_error;
pub mod commands;
pub mod context_builder;
pub mod credentials;
pub mod events;
pub mod git_status;
pub mod java;
pub mod knowledge_graph;
pub mod kotlin;
pub mod openai_provider;
pub mod persistence;
pub mod project_detection;
pub mod review_finding;
pub mod scanner;
pub mod spring;
pub mod state;
pub mod subscription_auth;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(state::AppState::default())
        .manage(commands::context::ReviewContextPreviewState::default())
        .manage(subscription_auth::SubscriptionLoginControl::default())
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
            commands::ai_review::approve_review_transfer,
            commands::ai_review::run_ai_review,
            commands::credentials::list_provider_credentials,
            commands::credentials::save_provider_credential,
            commands::credentials::delete_provider_credential,
            subscription_auth::sign_in_openai_subscription,
            subscription_auth::cancel_openai_subscription_sign_in,
            subscription_auth::disconnect_openai_subscription,
            subscription_auth::get_openai_subscription_status,
            commands::findings::validate_review_findings,
            commands::fix_tasks::create_fix_task,
            commands::fix_tasks::prepare_fix_task_workspace,
            commands::fix_tasks::list_fix_tasks,
            commands::fix_tasks::update_fix_task,
            commands::context::preview_review_context,
            commands::review_persistence::save_review_session,
            commands::review_persistence::list_review_sessions,
            commands::review_persistence::load_review_session,
            commands::review_persistence::set_finding_decision,
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
