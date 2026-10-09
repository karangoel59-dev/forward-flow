use crate::{
    ai::{providers, Connection},
    config::{self, GitSettings},
    mcp::{self, Server},
    vault_git,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tauri::AppHandle;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PortableConfig {
    version: u32,
    #[serde(default, skip_serializing)]
    vault: Option<String>,
    #[serde(default)]
    git: GitSettings,
    #[serde(default)]
    models: BTreeMap<String, Connection>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, Server>,
}
fn parse(text: &str) -> Result<PortableConfig, String> {
    if text.len() > 1_000_000 {
        return Err("Config file exceeds 1 MB".into());
    }
    let mut cfg: PortableConfig = serde_json::from_str(text)
        .map_err(|_| "Invalid config file. Use the Forward Flow JSON format.")?;
    cfg.vault = None;
    if cfg.version != 1 {
        return Err("Unsupported config version".into());
    }
    if cfg.models.len() > 3 || cfg.mcp_servers.len() > 64 {
        return Err("Too many connections in config file".into());
    }
    if let Some(remote) = &cfg.git.remote {
        validate_remote(remote)?;
    }
    for (id, model) in &cfg.models {
        if id != &model.provider {
            return Err("Model keys must match their provider".into());
        }
        providers::validate(&model.provider, &model.model)?;
        if model.api_key.len() > 16_000 {
            return Err("API key is too long".into());
        }
    }
    for (id, server) in &mut cfg.mcp_servers {
        if id != &server.id {
            return Err("MCP keys must match their server ID".into());
        }
        mcp::validate_server(server)?;
        if server.token.len() > 16_000 {
            return Err("MCP token is too long".into());
        }
        // Tool schemas must come from the connected server, never an uploaded file.
        server.tools.clear();
    }
    Ok(cfg)
}
fn validate_remote(remote: &str) -> Result<(), String> {
    if remote.len() > 16_000 || remote.chars().any(|c| c.is_control()) || remote.starts_with('-') {
        return Err("Invalid Git remote".into());
    }
    if let Ok(url) = reqwest::Url::parse(remote) {
        if matches!(url.scheme(), "https" | "ssh")
            && url.host_str().is_some()
            && url.query().is_none()
            && url.fragment().is_none()
        {
            return Ok(());
        }
    } else if let Some((host, path)) = remote.split_once(':') {
        if host.contains('@') && !host.contains('/') && !path.is_empty() {
            return Ok(());
        }
    }
    Err("Use an HTTPS or SSH Git remote".into())
}
fn portable(cfg: config::Config, include_secrets: bool) -> PortableConfig {
    let mut result = PortableConfig {
        version: 1,
        vault: None,
        git: cfg.git,
        models: cfg.models,
        mcp_servers: cfg.mcp_servers,
    };
    for server in result.mcp_servers.values_mut() {
        server.tools.clear();
        if !include_secrets {
            server.token.clear();
        }
    }
    if !include_secrets {
        for model in result.models.values_mut() {
            model.api_key.clear();
        }
        if let Some(remote) = &mut result.git.remote {
            if let Ok(mut url) = reqwest::Url::parse(remote) {
                let _ = url.set_password(None);
                // HTTPS userinfo can contain a token as the username too.
                if url.scheme() == "https" {
                    let _ = url.set_username("");
                }
                url.set_query(None);
                url.set_fragment(None);
                *remote = url.to_string();
            }
        }
    }
    result
}
#[tauri::command]
pub(crate) fn export_settings(
    app: AppHandle,
    path: String,
    include_secrets: bool,
) -> Result<(), String> {
    let mut cfg = config::settings(&app)?;
    if let Some(vault) = &cfg.vault {
        cfg.git.remote = vault_git::get_remote(std::path::Path::new(vault)).or(cfg.git.remote);
    }
    let data = serde_json::to_vec_pretty(&portable(cfg, include_secrets))
        .map_err(|_| "Cannot export config")?;
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        if std::path::Path::new(&path).exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "Cannot protect exported config")?;
        }
    }
    options
        .open(path)
        .and_then(|mut f| f.write_all(&data))
        .map_err(|_| "Cannot save exported config".into())
}
#[tauri::command]
pub(crate) fn import_settings(app: AppHandle, text: String) -> Result<String, String> {
    let imported = parse(&text)?;
    config::update(&app, |cfg| {
        if let (Some(vault), Some(remote)) = (&cfg.vault, &imported.git.remote) {
            // Configure locally; importing does not push or contact an external service.
            vault_git::configure_remote(std::path::Path::new(vault), remote)?;
        }
        if imported.git.remote.is_some() {
            cfg.git = imported.git;
        }
        for (id, mut connection) in imported.models {
            if connection.api_key.is_empty() {
                connection.api_key = cfg
                    .models
                    .get(&id)
                    .map(|c| c.api_key.clone())
                    .unwrap_or_default();
            }
            cfg.models.insert(id, connection);
        }
        for (id, mut server) in imported.mcp_servers {
            if let Some(old) = cfg.mcp_servers.get(&id).filter(|old| old.url == server.url) {
                if server.token.is_empty() {
                    server.token = old.token.clone();
                }
                server.tools = old.tools.clone();
            }
            cfg.mcp_servers.insert(id, server);
        }
        Ok(())
    })?;
    Ok("Config imported. Connect / refresh new MCP servers to discover tools. No sync was started.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn import_validates_every_connection_and_discards_uploaded_tools() {
        let mut value = json!({"version":1,"git":{"remote":"https://user:token@github.com/a/b.git"},"models":{"openai":{"provider":"openai","model":"gpt-4.1-mini","api_key":"secret"}},"mcp_servers":{"search":{"id":"search","url":"https://example.com/mcp","enabled":true,"tools":[{"name":"fake","inputSchema":{}}]}}});
        assert!(parse(&value.to_string()).unwrap().mcp_servers["search"]
            .tools
            .is_empty());
        value["mcp_servers"]["search"]["url"] = json!("http://example.com/mcp");
        assert!(parse(&value.to_string()).is_err());
        assert!(parse(r#"{"version":2}"#).is_err());
        assert!(
            parse(r#"{"version":1,"models":{"openai":{"provider":"claude","model":"test"}}}"#)
                .is_err()
        );
        assert!(parse(r#"{"version":1,"git":{"remote":"file:///tmp/repo"}}"#).is_err());
    }
    #[test]
    fn example_and_device_config_are_portable() {
        let example = include_str!("../../../examples/forward-flow-config.json");
        assert_eq!(parse(example).unwrap().models.len(), 3);
        let mut device = config::Config::default();
        device.vault = Some("/another/device/vault".into());
        let imported = parse(&serde_json::to_string(&device).unwrap()).unwrap();
        assert!(imported.vault.is_none());
    }
    #[test]
    fn export_excludes_device_paths_and_secrets_unless_requested() {
        let mut cfg = config::Config::default();
        cfg.vault = Some("/private/vault".into());
        cfg.git.remote = Some("https://token@github.com/a/b.git".into());
        cfg.models.insert(
            "openai".into(),
            Connection {
                provider: "openai".into(),
                model: "gpt-4.1-mini".into(),
                api_key: "private-key".into(),
            },
        );
        cfg.mcp_servers.insert(
            "search".into(),
            Server {
                id: "search".into(),
                url: "https://example.com/mcp".into(),
                token: "private-token".into(),
                enabled: true,
                tools: vec![],
            },
        );
        let safe = serde_json::to_string(&portable(cfg.clone(), false)).unwrap();
        assert!(!safe.contains("private"));
        assert!(!safe.contains("token@"));
        let full = serde_json::to_string(&portable(cfg, true)).unwrap();
        assert!(full.contains("private-key"));
        assert!(full.contains("private-token"));
        assert!(full.contains("token@"));
        assert!(!full.contains("/private/vault"));
    }
}
