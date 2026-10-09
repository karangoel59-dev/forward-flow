use std::{fs, path::PathBuf};
use tauri::{AppHandle, Manager};
pub(crate) fn draft_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("draft.md"))
}

#[tauri::command]
pub(crate) fn save_draft(app: AppHandle, content: String) -> Result<(), String> {
    fs::write(draft_path(&app)?, content).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn load_draft(app: AppHandle) -> String {
    draft_path(&app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default()
}
