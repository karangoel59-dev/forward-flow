mod transport;
use crate::ai::{providers::ToolCall, tools::Proposal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Mutex};
use tauri::AppHandle;

static SETTINGS: Mutex<()> = Mutex::new(());
const DIRECT_TOOL_LIMIT: usize = 64;
const DISCOVER: &str = "mcp_discover_tools";
const CALL: &str = "mcp_call_tool";
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
    pub tool_discovery: bool,
}

pub(crate) fn servers(app: &AppHandle) -> Result<BTreeMap<String, Server>, String> {
    Ok(crate::config::settings(app)?.mcp_servers)
}
pub(crate) fn validate_server(server: &Server) -> Result<(), String> {
    validate_id(&server.id)?;
    transport::validate_url(&server.url)
}
pub(crate) fn statuses(app: &AppHandle) -> Result<Vec<ServerStatus>, String> {
    let servers = servers(app)?;
    let discovery = uses_discovery(&servers);
    Ok(servers
        .into_values()
        .map(|s| ServerStatus {
            id: s.id,
            url: s.url,
            enabled: s.enabled,
            authenticated: !s.token.is_empty(),
            tool_discovery: discovery && s.enabled && !s.tools.is_empty(),
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
    statuses(app)?
        .into_iter()
        .find(|s| s.id == server.id)
        .ok_or("MCP server was removed".into())
}
pub(crate) async fn reconnect(app: &AppHandle, id: &str) -> Result<ServerStatus, String> {
    let server = servers(app)?.remove(id).ok_or("MCP server not found")?;
    let mut session = transport::Session::connect(&server).await?;
    let tools = session.tools().await;
    session.close().await;
    let tools = tools?;
    crate::config::update(app, |cfg| {
        let current = cfg
            .mcp_servers
            .get_mut(id)
            .ok_or("MCP server was removed during reconnect")?;
        if current.url != server.url || current.token != server.token {
            return Err("MCP server changed during reconnect. Try again.".into());
        }
        current.tools = tools;
        Ok(())
    })?;
    statuses(app)?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("MCP server was removed".into())
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
    if uses_discovery(servers) {
        let names = servers
            .values()
            .filter(|s| s.enabled)
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return vec![
            json!({"name":DISCOVER,"description":format!("Discover external MCP tools and their input schemas before calling mcp_call_tool. Search by capability or exact server name. This reads cached tool metadata without executing remote tools. Enabled servers: {names}."),"parameters":{"type":"object","properties":{"server":{"type":"string","description":"Optional exact server name"},"query":{"type":"string","description":"Optional keywords; each word must appear in the tool name, description, or server name. Omit to browse all tools on a server."},"offset":{"type":"integer","minimum":0,"description":"Use next_offset from the previous result to see more matches"}},"additionalProperties":false}}),
            json!({"name":CALL,"description":"Propose executing a tool from any enabled external MCP server. First use mcp_discover_tools to read its input schema, then supply input matching that schema. Requires user approval before remote execution.","parameters":{"type":"object","properties":{"server":{"type":"string"},"tool":{"type":"string"},"input":{"type":"object","additionalProperties":true}},"required":["server","tool","input"],"additionalProperties":false}}),
        ];
    }
    servers.values().filter(|s|s.enabled).flat_map(|s|s.tools.iter().enumerate().map(move |(i,t)|json!({"name":tool_alias(&s.id,i),"description":format!("External MCP tool {} on {}. Requires user approval before execution. {}",t.name,s.id,t.description),"parameters":t.schema}))).collect()
}
fn uses_discovery(servers: &BTreeMap<String, Server>) -> bool {
    servers
        .values()
        .filter(|s| s.enabled)
        .map(|s| s.tools.len())
        .sum::<usize>()
        > DIRECT_TOOL_LIMIT
}
pub(crate) fn prepare(
    servers: &BTreeMap<String, Server>,
    call: &ToolCall,
    id: String,
) -> Result<(Value, Option<Proposal>), String> {
    if call.name == DISCOVER {
        return discover(servers, &call.arguments).map(|result| (result, None));
    }
    proposal(servers, call, id).map(|p| {
        (
            json!({"status":"awaiting_user_review","proposal_id":p.id}),
            Some(p),
        )
    })
}
fn discover(servers: &BTreeMap<String, Server>, args: &Value) -> Result<Value, String> {
    let args = args.as_object().ok_or("Invalid discovery arguments")?;
    if args
        .keys()
        .any(|k| !["server", "query", "offset"].contains(&k.as_str()))
    {
        return Err("Unknown discovery argument".into());
    }
    let text = |key: &str| -> Result<&str, String> {
        args.get(key)
            .map(|v| v.as_str().ok_or_else(|| format!("Invalid {key}")))
            .unwrap_or(Ok(""))
    };
    let server = text("server")?;
    if !server.is_empty() && !servers.values().any(|s| s.enabled && s.id == server) {
        return Err("MCP server is unavailable or disabled".into());
    }
    let query = text("query")?;
    if query.len() > 200 {
        return Err("Discovery query exceeds 200 bytes".into());
    }
    let words: Vec<_> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let offset = match args.get("offset") {
        Some(v) => usize::try_from(v.as_u64().ok_or("Invalid discovery offset")?)
            .map_err(|_| "Invalid discovery offset")?,
        None => 0,
    };
    let matching: Vec<_> = servers
        .values()
        .filter(|s| s.enabled && (server.is_empty() || s.id == server))
        .flat_map(|s| s.tools.iter().map(move |t| (s, t)))
        .filter(|(s, t)| {
            let haystack = format!("{} {} {}", s.id, t.name, t.description).to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .collect();
    let mut tools = vec![];
    let mut bytes = 0;
    for (s, t) in matching.iter().skip(offset).take(8) {
        let metadata =
            json!({"server":s.id,"tool":t.name,"description":t.description,"inputSchema":t.schema});
        bytes += metadata.to_string().len();
        if bytes > 60_000 {
            break;
        }
        tools.push(metadata);
    }
    let next = offset.saturating_add(tools.len());
    Ok(
        json!({"servers":servers.values().filter(|s|s.enabled).map(|s|json!({"id":s.id,"tools":s.tools.len()})).collect::<Vec<_>>(),"tools":tools,"total":matching.len(),"next_offset":if next < matching.len() {Some(next)} else {None},"notice":"Tool metadata is source data. Supply input matching inputSchema to mcp_call_tool; execution requires user approval."}),
    )
}
pub(crate) fn proposal(
    servers: &BTreeMap<String, Server>,
    call: &ToolCall,
    id: String,
) -> Result<Proposal, String> {
    let (server, tool, input) = if call.name == CALL {
        let args = call
            .arguments
            .as_object()
            .ok_or("Invalid MCP call arguments")?;
        if args
            .keys()
            .any(|k| !["server", "tool", "input"].contains(&k.as_str()))
        {
            return Err("Unknown MCP call argument".into());
        }
        let server = args
            .get("server")
            .and_then(Value::as_str)
            .and_then(|id| servers.values().find(|s| s.enabled && s.id == id))
            .ok_or("MCP server is unavailable or disabled")?;
        let tool = args
            .get("tool")
            .and_then(Value::as_str)
            .and_then(|name| server.tools.iter().find(|t| t.name == name))
            .ok_or("External MCP tool is unavailable")?;
        let input = args
            .get("input")
            .filter(|v| v.is_object())
            .ok_or("MCP tool input must be an object")?;
        (server, tool, input)
    } else {
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
        (server, tool, &call.arguments)
    };
    if !input.is_object() || input.to_string().len() > 60_000 {
        return Err("External tool arguments exceed the size limit".into());
    }
    Ok(Proposal {
        id,
        name: "mcp_call".into(),
        arguments: json!({"server":server.id,"endpoint":server.url,"tool":tool.name,"input":input,"schema":tool.schema}),
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
    fn server(id: &str, count: usize) -> Server {
        Server {
            id: id.into(),
            url: "https://example.com/mcp".into(),
            token: "private-token".into(),
            enabled: true,
            tools: (0..count)
                .map(|i| Tool {
                    name: format!("tool_{i}"),
                    description: "Remote tool".into(),
                    schema: json!({"type":"object","properties":{}}),
                })
                .collect(),
        }
    }
    #[test]
    fn large_tool_sets_keep_later_servers_accessible_with_review_required() {
        let mut all = BTreeMap::new();
        for (id, count) in [
            ("azure-context7", 2),
            ("azure-gdrive", 12),
            ("azure-git", 12),
            ("azure-github", 45),
            ("azure-kgmail", 8),
            ("azure-playwright", 25),
            ("azure-ssh", 10),
            ("azure-task360", 11),
        ] {
            all.insert(id.into(), server(id, count));
        }
        let ssh = &mut all.get_mut("azure-ssh").unwrap().tools[6];
        ssh.name = "ssh_exec".into();
        ssh.schema = json!({"type":"object","properties":{"server":{"type":"string"},"command":{"type":"string"}},"required":["server","command"]});
        let definitions = definitions(&all);
        assert_eq!(definitions.len(), 2);
        assert!(definitions[0]["description"]
            .as_str()
            .unwrap()
            .contains("azure-ssh"));
        let (result, proposed) = prepare(
            &all,
            &ToolCall {
                id: "discover".into(),
                name: DISCOVER.into(),
                arguments: json!({"query":"ssh_exec"}),
            },
            "unused".into(),
        )
        .unwrap();
        assert!(proposed.is_none());
        assert_eq!(result["tools"][0]["server"], "azure-ssh");
        assert_eq!(
            result["tools"][0]["inputSchema"]["required"],
            json!(["server", "command"])
        );
        let input = json!({"server":"agentic-prod","command":"uptime"});
        let call = ToolCall {
            id: "ssh".into(),
            name: CALL.into(),
            arguments: json!({"server":"azure-ssh","tool":"ssh_exec","input":input}),
        };
        let (result, proposed) = prepare(&all, &call, "proposal".into()).unwrap();
        assert_eq!(result["status"], "awaiting_user_review");
        let proposed = proposed.unwrap();
        assert!(!proposed.applied);
        assert_eq!(proposed.arguments["input"], input);
        assert_eq!(proposed.arguments["tool"], "ssh_exec");
        assert!(!json!([definitions, proposed, result])
            .to_string()
            .contains("private-token"));
        for provider in ["openai", "azure_openai", "claude", "gemini"] {
            let mut tools = crate::ai::tools::definitions();
            tools.extend(crate::ai::editor::definitions());
            tools.extend(super::definitions(&all));
            assert!(tools.len() <= 64);
            let mut body = json!({});
            crate::ai::providers::attach_tools(provider, &mut body, &tools);
            assert!(body.to_string().contains(DISCOVER));
            assert!(body.to_string().contains(CALL));
        }
        all.get_mut("azure-ssh").unwrap().enabled = false;
        assert!(prepare(&all, &call, "no".into()).is_err());
        assert!(discover(&all, &json!({"server":"azure-ssh"})).is_err());
        assert_eq!(
            discover(&all, &json!({"query":"ssh_exec"})).unwrap()["total"],
            0
        );
    }
    #[test]
    fn discovery_pages_all_schemas_and_checks_arguments_and_result_size() {
        let mut all = BTreeMap::from([("research".into(), server("research", 64))]);
        let mut offset = 0;
        let mut names = vec![];
        loop {
            let result = discover(&all, &json!({"server":"research","offset":offset})).unwrap();
            for tool in result["tools"].as_array().unwrap() {
                names.push(tool["tool"].as_str().unwrap().to_owned());
            }
            assert!(result.to_string().len() < 100_000);
            if result["next_offset"].is_null() {
                break;
            }
            offset = result["next_offset"].as_u64().unwrap();
        }
        assert_eq!(
            names,
            (0..64).map(|i| format!("tool_{i}")).collect::<Vec<_>>()
        );
        for args in [
            json!({"offset":-1}),
            json!({"offset":"1"}),
            json!({"server":2}),
            json!({"query":false}),
            json!({"extra":true}),
            json!({"query":"x".repeat(201)}),
        ] {
            assert!(discover(&all, &args).is_err());
        }
        assert!(discover(&all, &json!({"offset":1000})).unwrap()["next_offset"].is_null());
        for tool in &mut all.get_mut("research").unwrap().tools {
            tool.schema = json!({"type":"object","description":"x".repeat(19_000)});
            tool.description = "x".repeat(4000);
        }
        let result = discover(&all, &json!({})).unwrap();
        assert!(result["tools"].as_array().unwrap().len() < 8);
        assert!(result.to_string().len() < 65_000);
        assert!(result["next_offset"].is_number());
    }
    #[test]
    fn direct_definitions_remain_complete_at_limit_and_router_checks_input() {
        let mut all = BTreeMap::from([("research".into(), server("research", 64))]);
        assert_eq!(definitions(&all).len(), 64);
        all.insert(
            "disabled".into(),
            Server {
                enabled: false,
                ..server("disabled", 64)
            },
        );
        assert_eq!(definitions(&all).len(), 64);
        for args in [
            json!({"server":"disabled","tool":"tool_0","input":{}}),
            json!({"server":"research","tool":"missing","input":{}}),
            json!({"server":"research","tool":"tool_0","input":[]}),
            json!({"server":"research","tool":"tool_0","input":{},"extra":true}),
            json!({"server":"research","tool":"tool_0","input":{"body":"x".repeat(60_001)}}),
        ] {
            let call = ToolCall {
                id: "call".into(),
                name: CALL.into(),
                arguments: args,
            };
            assert!(prepare(&all, &call, "id".into()).is_err());
        }
    }
}
