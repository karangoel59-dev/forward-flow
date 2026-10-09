use crate::{ai::Connection, mcp::Server};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Manager};

static SETTINGS: Mutex<()> = Mutex::new(());
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub(crate) struct Config {
    pub(crate) version: u32,
    pub(crate) vault: Option<String>,
    pub(crate) git: GitSettings,
    pub(crate) models: BTreeMap<String, Connection>,
    pub(crate) mcp_servers: BTreeMap<String, Server>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            vault: None,
            git: GitSettings::default(),
            models: BTreeMap::new(),
            mcp_servers: BTreeMap::new(),
        }
    }
}
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GitSettings {
    pub remote: Option<String>,
}
fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|_| "Cannot access app settings")?;
    fs::create_dir_all(&dir).map_err(|_| "Cannot create app settings")?;
    Ok(dir.join("config.json"))
}
fn load(app: &AppHandle) -> Result<Config, String> {
    load_at(&config_path(app)?)
}
fn load_at(path: &std::path::Path) -> Result<Config, String> {
    let raw = if path.exists() {
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(&path).map_err(|_| "Cannot read settings")?,
        )
        .map_err(|_| "Invalid config.json")?
    } else {
        serde_json::json!({})
    };
    let mut cfg: Config = serde_json::from_value(raw.clone()).map_err(|_| "Invalid config.json")?;
    if cfg.version != 1 {
        return Err("Unsupported config version".into());
    }
    let mut migrated = false;
    for (field, filename) in [
        ("models", "ai-connections.json"),
        ("mcp_servers", "mcp-servers.json"),
    ] {
        let legacy = path.with_file_name(filename);
        if raw.get(field).is_none() && legacy.exists() {
            let data = fs::read(&legacy).map_err(|_| "Cannot read legacy settings")?;
            match field {
                "models" => {
                    cfg.models =
                        serde_json::from_slice(&data).map_err(|_| "Invalid legacy AI settings")?
                }
                _ => {
                    cfg.mcp_servers =
                        serde_json::from_slice(&data).map_err(|_| "Invalid legacy MCP settings")?
                }
            }
            migrated = true;
        }
    }
    if raw.get("git").is_none() {
        if let Some(vault) = &cfg.vault {
            cfg.git.remote = crate::vault_git::get_remote(std::path::Path::new(vault));
            migrated |= cfg.git.remote.is_some();
        }
    }
    if migrated {
        persist(&path, &cfg)?;
        // Only delete the old files after the combined file is safely saved.
        for filename in ["ai-connections.json", "mcp-servers.json"] {
            let _ = fs::remove_file(path.with_file_name(filename));
        }
    }
    Ok(cfg)
}
fn persist(path: &std::path::Path, cfg: &Config) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        if temp.exists() {
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
                .map_err(|_| "Cannot protect settings")?;
        }
    }
    let body = serde_json::to_vec_pretty(cfg).map_err(|_| "Cannot serialize settings")?;
    let mut file = options.open(&temp).map_err(|_| "Cannot save settings")?;
    file.write_all(&body)
        .and_then(|_| file.sync_all())
        .map_err(|_| "Cannot save settings")?;
    fs::rename(&temp, path).map_err(|_| "Cannot replace settings".into())
}
pub(crate) fn settings(app: &AppHandle) -> Result<Config, String> {
    let _lock = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    load(app)
}
pub(crate) fn read_config(app: &AppHandle) -> Config {
    settings(app).unwrap_or_default()
}
pub(crate) fn update(
    app: &AppHandle,
    change: impl FnOnce(&mut Config) -> Result<(), String>,
) -> Result<(), String> {
    let _lock = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    let mut cfg = load(app)?;
    change(&mut cfg)?;
    persist(&config_path(app)?, &cfg)
}
pub(crate) fn vault_dir(app: &AppHandle) -> Result<PathBuf, String> {
    settings(app)?
        .vault
        .map(PathBuf::from)
        .ok_or_else(|| "no vault selected".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_connections_migrate_once_and_settings_are_private() {
        let dir = std::env::temp_dir().join(format!(
            "ff-config-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(&path, r#"{"vault":null}"#).unwrap();
        fs::write(
            dir.join("ai-connections.json"),
            r#"{"openai":{"provider":"openai","model":"gpt-4.1-mini","api_key":"secret"}}"#,
        )
        .unwrap();
        fs::write(dir.join("mcp-servers.json"), r#"{"research":{"id":"research","url":"https://example.com/mcp","token":"token","enabled":true}}"#).unwrap();
        let cfg = load_at(&path).unwrap();
        assert_eq!(cfg.models["openai"].api_key, "secret");
        assert_eq!(cfg.mcp_servers["research"].token, "token");
        assert!(!dir.join("ai-connections.json").exists());
        assert!(!dir.join("mcp-servers.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut cfg = cfg;
        cfg.models.clear();
        persist(&path, &cfg).unwrap();
        fs::write(dir.join("ai-connections.json"), "invalid legacy file").unwrap();
        assert!(load_at(&path).unwrap().models.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn corrupt_config_is_not_replaced_by_defaults() {
        let dir = std::env::temp_dir().join(format!("ff-bad-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(&path, "broken").unwrap();
        assert!(load_at(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "broken");
        fs::remove_dir_all(dir).unwrap();
    }
}
