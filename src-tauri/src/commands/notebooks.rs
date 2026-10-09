use super::entries::entry_in_vault;
use crate::{
    config::vault_dir,
    entries::{meta_from, EntryMeta},
    notebooks::{collect_notebooks, create_notebook_folder, move_entry_file},
    state::VAULT_WRITES,
    vault_git,
};
use std::fs;
use tauri::AppHandle;
#[tauri::command]
pub(crate) fn list_notebooks(app: AppHandle) -> Result<Vec<String>, String> {
    collect_notebooks(&vault_dir(&app)?)
}

#[tauri::command]
pub(crate) fn create_notebook(app: AppHandle, name: String) -> Result<String, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let root = vault_dir(&app)?;
    let name = create_notebook_folder(&root, &name)?;
    vault_git::record(&app, root, format!("Create notebook {name}"), true);
    Ok(name)
}

#[tauri::command]
pub(crate) fn move_entry(
    app: AppHandle,
    path: String,
    notebook: String,
) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let source = entry_in_vault(&app, &path)?;
    let root = vault_dir(&app)?;
    let raw = fs::read_to_string(&source).map_err(|e| e.to_string())?;
    let destination = move_entry_file(&root, &source, &notebook)?;
    let mut meta = meta_from(&destination, &raw);
    meta.notebook = notebook;
    vault_git::record(
        &app,
        root,
        format!("Move entry {} to notebook {}", meta.name, meta.notebook),
        true,
    );
    Ok(meta)
}
