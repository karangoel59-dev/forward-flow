use crate::mcp::{self, Server, ServerStatus};
use tauri::AppHandle;
#[tauri::command]
pub(crate) fn list_mcp_servers(app: AppHandle) -> Result<Vec<ServerStatus>, String> {
    mcp::statuses(&app)
}
#[tauri::command]
pub(crate) async fn connect_mcp_server(
    app: AppHandle,
    server: Server,
) -> Result<ServerStatus, String> {
    mcp::connect(&app, server).await
}
#[tauri::command]
pub(crate) fn remove_mcp_server(app: AppHandle, id: String) -> Result<(), String> {
    mcp::remove(&app, &id)
}
#[tauri::command]
pub(crate) fn enable_mcp_server(app: AppHandle, id: String, enabled: bool) -> Result<(), String> {
    mcp::enable(&app, &id, enabled)
}
