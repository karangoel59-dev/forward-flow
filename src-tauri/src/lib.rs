mod ai;
mod commands;
mod config;
mod drafts;
mod entries;
mod mcp;
mod notebooks;
mod state;
mod vault_git;

#[cfg(all(target_os = "android", feature = "tls-diagnostics"))]
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(all(target_os = "android", feature = "tls-diagnostics"))]
            if let Ok(dir) = app.path().app_data_dir() {
                vault_git::diagnose_tls(dir);
            }
            // Commit external changes and retry pending pushes on launch.
            if let Ok(vault) = config::vault_dir(app.handle()) {
                vault_git::record(app.handle(), vault.clone(), "Sync vault".into(), false);
            }
            vault_git::start_background_sync(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::settings::import_settings,
            commands::settings::export_settings,
            commands::vault::get_vault,
            commands::vault::set_vault,
            commands::vault::get_remote,
            commands::vault::set_remote,
            commands::vault::resync_vault,
            commands::entries::list_entries,
            commands::notebooks::list_notebooks,
            commands::notebooks::create_notebook,
            commands::notebooks::move_entry,
            commands::entries::read_entry,
            commands::entries::commit_entry,
            commands::entries::delete_entry,
            commands::entries::set_tags,
            commands::entries::link_entries,
            commands::entries::unlink_entries,
            commands::entries::all_tags,
            drafts::save_draft,
            drafts::load_draft,
            commands::chat::list_ai_models,
            commands::chat::set_ai_model,
            commands::chat::save_ai_model,
            commands::chat::list_notebook_chats,
            commands::chat::resume_notebook_chat,
            commands::chat::get_ai_connections,
            commands::chat::set_ai_connection,
            commands::chat::get_notebook_chat,
            commands::chat::set_notebook_purpose,
            commands::chat::clear_notebook_chat,
            commands::chat::chat_notebook,
            commands::chat::apply_chat_proposal,
            commands::mcp::reconnect_mcp_server,
            commands::mcp::list_mcp_servers,
            commands::mcp::connect_mcp_server,
            commands::mcp::remove_mcp_server,
            commands::mcp::enable_mcp_server,
            commands::entries::save_chat_page
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
