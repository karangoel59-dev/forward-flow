mod transport;
use crate::ai::{providers::ToolCall, tools::Proposal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Mutex};
use tauri::AppHandle;

static SETTINGS: Mutex<()> = Mutex::new(());
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Server {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tools: Vec<Tool>,
}
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub schema: Value,
}
#[derive(Serialize)]
pub(crate) struct ServerStatus {
    pub id: String,
    pub url: String,
    pub enabled: bool,
    pub authenticated: bool,
    pub tools: Vec<Tool>,
}

pub(crate) fn servers(app: &AppHandle) -> Result<BTreeMap<String, Server>, String> {
    Ok(crate::config::settings(app)?.mcp_servers)
}
pub(crate) fn validate_server(server: &Server) -> Result<(), String> {
    validate_id(&server.id)?;
    transport::validate_url(&server.url)
}
pub(crate) fn statuses(app: &AppHandle) -> Result<Vec<ServerStatus>, String> {
    Ok(servers(app)?
        .into_values()
        .map(|s| ServerStatus {
            id: s.id,
            url: s.url,
            enabled: s.enabled,
            authenticated: !s.token.is_empty(),
            tools: s.tools,
        })
        .collect())
}
pub(crate) async fn connect(app: &AppHandle, mut server: Server) -> Result<ServerStatus, String> {
    validate_id(&server.id)?;
    transport::validate_url(&server.url)?;
    if server.token.is_empty() {
        if let Some(old) = servers(app)?.get(&server.id) {
            if old.url == server.url {
                server.token = old.token.clone();
            }
        }
    }
    let mut session = transport::Session::connect(&server).await?;
    let discovered = session.tools().await;
    session.close().await;
    server.tools = discovered?;
    let _lock = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    crate::config::update(app, |cfg| {
        cfg.mcp_servers.insert(server.id.clone(), server.clone());
        Ok(())
    })?;
    Ok(ServerStatus {
        id: server.id,
        url: server.url,
        enabled: server.enabled,
        authenticated: !server.token.is_empty(),
        tools: server.tools,
    })
}
pub(crate) fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    let _lock = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    crate::config::update(app, |cfg| {
        cfg.mcp_servers.remove(id);
        Ok(())
    })
}
pub(crate) fn enable(app: &AppHandle, id: &str, enabled: bool) -> Result<(), String> {
    let _lock = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    crate::config::update(app, |cfg| {
        cfg.mcp_servers
            .get_mut(id)
            .ok_or("MCP server not found")?
            .enabled = enabled;
        Ok(())
    })
}
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 24 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("Use a server name of up to 24 letters, numbers, or hyphens".into());
    }
    Ok(())
}
fn tool_alias(server: &str, index: usize) -> String {
    let encoded: String = server.bytes().map(|b| format!("{b:02x}")).collect();
    format!("mcp_{encoded}_{index}")
}
pub(crate) fn definitions(servers: &BTreeMap<String, Server>) -> Vec<Value> {
    servers.values().filter(|s|s.enabled).flat_map(|s|s.tools.iter().enumerate().map(move |(i,t)|json!({"name":tool_alias(&s.id,i),"description":format!("External MCP tool {} on {}. Requires user approval before execution. {}",t.name,s.id,t.description),"parameters":t.schema}))).take(64).collect()
}
pub(crate) fn proposal(
    servers: &BTreeMap<String, Server>,
    call: &ToolCall,
    id: String,
) -> Result<Proposal, String> {
    let (server, tool) = servers
        .values()
        .filter(|s| s.enabled)
        .find_map(|s| {
            s.tools
                .iter()
                .enumerate()
                .find(|(i, _)| tool_alias(&s.id, *i) == call.name)
                .map(|(_, t)| (s, t))
        })
        .ok_or("External MCP tool is unavailable")?;
    if call.arguments.to_string().len() > 60_000 {
        return Err("External tool arguments exceed the size limit".into());
    }
    Ok(Proposal {
        id,
        name: "mcp_call".into(),
        arguments: json!({"server":server.id,"endpoint":server.url,"tool":tool.name,"input":call.arguments,"schema":tool.schema}),
        before: vec![],
        applied: false,
    })
}
pub(crate) async fn execute(app: &AppHandle, proposal: &Proposal) -> Result<Value, String> {
    let args = &proposal.arguments;
    let server = servers(app)?
        .remove(args["server"].as_str().ok_or("Invalid MCP server")?)
        .ok_or("MCP server was removed")?;
    if !server.enabled || args["endpoint"] != server.url {
        return Err("MCP server changed or was disabled. Request a new proposal.".into());
    }
    let name = args["tool"].as_str().ok_or("Invalid MCP tool")?;
    let mut session = transport::Session::connect(&server).await?;
    let outcome = async {
        let current = session.tools().await?;
        let tool = current
            .iter()
            .find(|t| t.name == name)
            .ok_or("MCP tool is no longer available")?;
        if tool.schema != args["schema"] {
            return Err("MCP tool schema changed. Reconnect and request a new proposal.".into());
        }
        session.call(name, &args["input"]).await
    }
    .await;
    session.close().await;
    outcome.map(|result| {
        if server.token.is_empty() {
            return result;
        }
        serde_json::from_str(&result.to_string().replace(&server.token, "[redacted]"))
            .unwrap_or(json!({"error":"Cannot sanitize MCP output"}))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovered_tools_are_namespaced_and_external_calls_only_propose() {
        let server = Server {
            id: "research".into(),
            url: "https://example.com/mcp".into(),
            token: "private-token".into(),
            enabled: true,
            tools: vec![Tool {
                name: "read_entry".into(),
                description: "Remote search".into(),
                schema: json!({"type":"object","properties":{}}),
            }],
        };
        let mut all = BTreeMap::new();
        all.insert(server.id.clone(), server);
        let definitions = definitions(&all);
        assert_ne!(definitions[0]["name"], "read_entry");
        assert!(!serde_json::to_string(&definitions)
            .unwrap()
            .contains("private-token"));
        let call = ToolCall {
            id: "call".into(),
            name: definitions[0]["name"].as_str().unwrap().into(),
            arguments: json!({}),
        };
        let proposed = proposal(&all, &call, "proposal".into()).unwrap();
        assert!(!proposed.applied);
        assert_eq!(proposed.arguments["tool"], "read_entry");
        assert!(!serde_json::to_string(&proposed)
            .unwrap()
            .contains("private-token"));
        all.get_mut("research").unwrap().enabled = false;
        assert!(proposal(&all, &call, "proposal".into()).is_err());
    }
    #[test]
    fn tool_aliases_do_not_collide() {
        assert_ne!(tool_alias("a-b", 1), tool_alias("ab", 1));
        assert!(tool_alias(&"x".repeat(24), 63).len() < 64);
    }
}
