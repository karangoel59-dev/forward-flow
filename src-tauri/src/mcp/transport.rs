use super::{Server, Tool};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::time::Duration;
const VERSION: &str = "2025-11-25";
const MAX_BYTES: usize = 512_000;

pub(super) fn validate_url(value: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|_| "Enter a full MCP endpoint URL")?;
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err("Use an HTTPS MCP endpoint (HTTP is supported for localhost). Put credentials in the token field, not the URL.".into());
    }
    Ok(())
}
pub(super) struct Session {
    client: Client,
    server: Server,
    session: Option<String>,
    version: String,
    next: u64,
}
impl Session {
    pub async fn connect(server: &Server) -> Result<Self, String> {
        validate_url(&server.url)?;
        let mut s = Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Cannot initialize MCP connection")?,
            server: server.clone(),
            session: None,
            version: VERSION.into(),
            next: 0,
        };
        let result=s.rpc("initialize",json!({"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"forward-flow","version":env!("CARGO_PKG_VERSION")}}),false).await?;
        let version = result["protocolVersion"]
            .as_str()
            .ok_or("MCP server omitted its protocol version")?;
        if !["2025-11-25", "2025-06-18", "2025-03-26"].contains(&version) {
            s.close().await;
            return Err("This MCP protocol version is unsupported; use a server supporting a 2025 Streamable HTTP version.".into());
        }
        s.version = version.into();
        if result["capabilities"].get("tools").is_none() {
            s.close().await;
            return Err("MCP server does not advertise tools".into());
        }
        s.rpc("notifications/initialized", json!({}), true).await?;
        Ok(s)
    }
    async fn rpc(
        &mut self,
        method: &str,
        params: Value,
        notification: bool,
    ) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let mut payload = json!({"jsonrpc":"2.0","method":method,"params":params});
        if !notification {
            payload["id"] = json!(id);
        }
        let mut request = self
            .client
            .post(&self.server.url)
            .header("Accept", "application/json, text/event-stream")
            .json(&payload);
        if method != "initialize" {
            request = request.header("MCP-Protocol-Version", &self.version);
        }
        if let Some(session) = &self.session {
            request = request.header("MCP-Session-Id", session);
        }
        if !self.server.token.is_empty() {
            request = request.bearer_auth(&self.server.token);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "MCP connection failed or timed out. Check the endpoint and network.")?;
        if !response.status().is_success() {
            return Err(format!(
                "MCP server returned HTTP {}. Check the endpoint, token, and server access.",
                response.status().as_u16()
            ));
        }
        if let Some(session) = response.headers().get("MCP-Session-Id") {
            self.session = Some(
                session
                    .to_str()
                    .map_err(|_| "Invalid MCP session header")?
                    .into(),
            );
        }
        if notification {
            return Ok(Value::Null);
        }
        let stream = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let mut bytes = vec![];
        let mut total = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "MCP response stream failed")?
        {
            total += chunk.len();
            if total > MAX_BYTES {
                return Err("MCP response exceeds the size limit".into());
            }
            bytes.extend_from_slice(&chunk);
            if stream {
                while let Some((end, sep)) = event_boundary(&bytes) {
                    let event = bytes.drain(..end + sep).collect::<Vec<_>>();
                    if let Some(message) = event_message(&event[..end])? {
                        if message["id"] == id {
                            return result(message, id);
                        }
                        if message.get("method").is_some() && message.get("id").is_some() {
                            return Err("This server requires unsupported client callbacks (sampling or elicitation)".into());
                        }
                    }
                }
            }
        }
        if stream {
            return Err("MCP stream ended before the matching response".into());
        }
        result(
            serde_json::from_slice(&bytes).map_err(|_| "Invalid MCP JSON response")?,
            id,
        )
    }
    pub async fn tools(&mut self) -> Result<Vec<Tool>, String> {
        let mut out = vec![];
        let mut cursor = None;
        let mut seen = vec![];
        for _ in 0..10 {
            let result = self
                .rpc(
                    "tools/list",
                    cursor
                        .as_ref()
                        .map(|c| json!({"cursor":c}))
                        .unwrap_or(json!({})),
                    false,
                )
                .await?;
            let tools: Vec<Tool> = serde_json::from_value(result["tools"].clone())
                .map_err(|_| "MCP returned an invalid tool list")?;
            for tool in tools {
                if tool.name.is_empty()
                    || tool.name.len() > 200
                    || tool.description.len() > 4000
                    || tool.schema["type"] != "object"
                    || tool.schema.to_string().len() > 20_000
                {
                    return Err(
                        "MCP tool metadata exceeds supported limits or has an invalid schema"
                            .into(),
                    );
                }
                if out.iter().any(|t: &Tool| t.name == tool.name) {
                    return Err("MCP returned duplicate tool names".into());
                }
                out.push(tool);
                if out.len() > 64 {
                    return Err(
                        "MCP server exposes more than 64 tools; limit its tool set first".into(),
                    );
                }
            }
            cursor = result["nextCursor"].as_str().map(str::to_owned);
            if let Some(c) = &cursor {
                if seen.contains(c) {
                    return Err("MCP repeated a pagination cursor".into());
                }
                seen.push(c.clone());
            } else {
                return Ok(out);
            }
        }
        Err("MCP tool pagination exceeded its limit".into())
    }
    pub async fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        if !args.is_object() {
            return Err("MCP tool arguments must be an object".into());
        }
        let result = self
            .rpc("tools/call", json!({"name":name,"arguments":args}), false)
            .await?;
        // Only text/structured output is passed to the model; embedded binaries stay on the server.
        let content: Vec<_> = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["type"] == "text")
            .cloned()
            .collect();
        let output = json!({"isError":result["isError"].as_bool().unwrap_or(false),"content":content,"structuredContent":result["structuredContent"]});
        if output.to_string().len() > 60_000 {
            return Err("MCP tool result exceeds 60 KB. The tool may have completed; check the server before retrying.".into());
        }
        Ok(output)
    }
    pub async fn close(&self) {
        if let Some(session) = &self.session {
            let mut request = self
                .client
                .delete(&self.server.url)
                .header("MCP-Session-Id", session)
                .header("MCP-Protocol-Version", &self.version)
                .timeout(Duration::from_secs(3));
            if !self.server.token.is_empty() {
                request = request.bearer_auth(&self.server.token);
            }
            let _ = request.send().await;
        }
    }
}
fn result(message: Value, id: u64) -> Result<Value, String> {
    if message["jsonrpc"] != "2.0" || message["id"] != id {
        return Err("MCP response does not match the request".into());
    }
    if message.get("error").is_some() {
        return Err(format!("MCP protocol error {}", message["error"]["code"]));
    }
    message
        .get("result")
        .cloned()
        .ok_or("MCP response has no result".into())
}
fn event_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}
fn event_message(bytes: &[u8]) -> Result<Option<Value>, String> {
    let event = std::str::from_utf8(bytes).map_err(|_| "MCP sent invalid UTF-8")?;
    let data = event
        .lines()
        .filter_map(|l| {
            l.strip_prefix("data:")
                .map(|v| v.strip_prefix(' ').unwrap_or(v))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|_| "Invalid MCP SSE data".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_reject_embedded_credentials_and_insecure_remote_http() {
        assert!(validate_url("https://example.com/mcp").is_ok());
        assert!(validate_url("http://127.0.0.1:8000/mcp").is_ok());
        for url in [
            "http://example.com/mcp",
            "https://user:token@example.com/mcp",
            "https://example.com/mcp?key=secret",
            "file:///tmp/server",
        ] {
            assert!(validate_url(url).is_err());
        }
    }
    #[test]
    fn sse_decodes_multiline_data_and_matches_ids() {
        let event=b"event: message\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":7,\"result\":{\"ok\":true}}\r\n\r\n";
        let (end, _) = event_boundary(event).unwrap();
        let message = event_message(&event[..end]).unwrap().unwrap();
        assert_eq!(result(message.clone(), 7).unwrap()["ok"], true);
        assert!(result(message, 8).is_err());
        assert!(result(json!({"jsonrpc":"2.0","id":7,"error":{"code":-32601}}), 7).is_err());
    }
    #[test]
    fn http_session_discovers_paginated_tools_and_calls_over_sse() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for index in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = vec![];
                let mut buf = [0u8; 4096];
                let (header_end, length) = loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while request.len() < header_end + length {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                }
                let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
                assert!(headers.contains("authorization: bearer private-token"));
                if index > 0 {
                    assert!(headers.contains("mcp-session-id: session1"));
                    assert!(headers.contains("mcp-protocol-version: 2025-06-18"));
                }
                let body: Value = if length > 0 {
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap()
                } else {
                    Value::Null
                };
                let (id, result, kind) = match index {
                    0 => {
                        assert_eq!(body["method"], "initialize");
                        (
                            1,
                            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}),
                            "application/json",
                        )
                    }
                    1 => {
                        assert_eq!(body["method"], "notifications/initialized");
                        (0, Value::Null, "application/json")
                    }
                    2 => (
                        3,
                        json!({"tools":[{"name":"search","description":"Search","inputSchema":{"type":"object","properties":{}}}],"nextCursor":"page2"}),
                        "application/json",
                    ),
                    3 => {
                        assert_eq!(body["params"]["cursor"], "page2");
                        (4, json!({"tools":[]}), "application/json")
                    }
                    4 => {
                        assert_eq!(body["method"], "tools/call");
                        assert_eq!(body["params"]["name"], "search");
                        (
                            5,
                            json!({"content":[{"type":"text","text":"Found an answer"}],"structuredContent":{"count":1}}),
                            "text/event-stream",
                        )
                    }
                    _ => {
                        assert!(headers.starts_with("delete"));
                        (0, Value::Null, "application/json")
                    }
                };
                let raw = if id == 0 {
                    String::new()
                } else {
                    let json = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                    if kind == "text/event-stream" {
                        format!("data: {json}\n\n")
                    } else {
                        json
                    }
                };
                let status = if id == 0 { "202 Accepted" } else { "200 OK" };
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nMCP-Session-Id: session1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}",raw.len()).unwrap();
            }
        });
        tauri::async_runtime::block_on(async {
            let config = Server {
                id: "test".into(),
                url: endpoint,
                token: "private-token".into(),
                enabled: true,
                tools: vec![],
            };
            let mut session = Session::connect(&config).await.unwrap();
            assert_eq!(session.tools().await.unwrap()[0].name, "search");
            let result = session.call("search", &json!({})).await.unwrap();
            assert_eq!(result["content"][0]["text"], "Found an answer");
            assert_eq!(result["structuredContent"]["count"], 1);
            session.close().await;
        });
        server.join().unwrap();
    }
}
