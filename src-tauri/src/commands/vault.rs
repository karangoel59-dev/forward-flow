use crate::{
    config::{read_config, update, vault_dir},
    vault_git,
};
use std::{fs, path::PathBuf};
use tauri::AppHandle;
#[cfg(target_os = "android")]
use tauri::Manager;
#[tauri::command]
pub(crate) fn get_vault(app: AppHandle) -> Option<String> {
    if let Some(v) = read_config(&app).vault {
        return Some(v);
    }

    // Android has no folder picker; use private app storage and git for transfer.
    #[cfg(target_os = "android")]
    {
        let dir = app.path().app_data_dir().ok()?.join("vault");
        fs::create_dir_all(&dir).ok()?;
        let path = dir.to_string_lossy().to_string();
        update(&app, |cfg| {
            cfg.vault = Some(path.clone());
            Ok(())
        })
        .ok()?;
        if let Some(remote) = read_config(&app).git.remote {
            vault_git::configure_remote(&dir, &remote).ok()?;
        }
        vault_git::record(&app, dir, "Start Forward Flow vault".into(), true);
        return Some(path);
    }

    #[cfg(not(target_os = "android"))]
    None
}

#[tauri::command]
pub(crate) fn set_vault(app: AppHandle, path: String) -> Result<(), String> {
    fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let cfg = crate::config::settings(&app)?;
    if let Some(remote) = cfg.git.remote {
        vault_git::configure_remote(&PathBuf::from(&path), &remote)?;
    }
    update(&app, |cfg| {
        cfg.vault = Some(path.clone());
        Ok(())
    })?;
    vault_git::record(
        &app,
        PathBuf::from(path),
        "Start Forward Flow vault".into(),
        true,
    );
    Ok(())
}

#[tauri::command]
pub(crate) fn get_remote(app: AppHandle) -> Option<String> {
    vault_git::get_remote(&vault_dir(&app).ok()?)
}

#[tauri::command]
pub(crate) fn set_remote(app: AppHandle, url: String) -> Result<(), String> {
    vault_git::set_remote(&app, vault_dir(&app)?, url.clone())?;
    update(&app, |cfg| {
        cfg.git.remote = Some(url);
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn resync_vault(app: AppHandle) -> Result<(), String> {
    let dir = vault_dir(&app)?;
    vault_git::sync_now(&app, dir, true);
    Ok(())
}

#[tauri::command]
pub(crate) async fn get_conflicts(app: AppHandle) -> Result<vault_git::Conflicts, String> {
    let dir = vault_dir(&app)?;
    // Fetching can be slow; keep it off the UI thread.
    tauri::async_runtime::spawn_blocking(move || vault_git::find_conflicts(&dir))
        .await
        .map_err(|_| "Cannot check for sync conflicts")?
}

#[tauri::command]
pub(crate) async fn resolve_conflicts(
    app: AppHandle,
    local: String,
    remote: String,
    choices: Vec<vault_git::Choice>,
) -> Result<(), String> {
    let dir = vault_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        vault_git::resolve_conflicts(&app, dir, &local, &remote, &choices)
    })
    .await
    .map_err(|_| "Cannot resolve sync conflicts")?
}
