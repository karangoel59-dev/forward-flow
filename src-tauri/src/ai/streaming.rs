use super::{providers, Connection};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_EVENT_BYTES: usize = 1_000_000;
const MAX_STREAM_BYTES: usize = 4_000_000;

fn request(provider: &str, url: &str, body: &Value) -> (String, Value) {
    let mut body = body.clone();
    if provider == "gemini" {
        (
            url.replace(":generateContent", ":streamGenerateContent?alt=sse"),
            body,
        )
    } else {
        body["stream"] = json!(true);
        (url.into(), body)
    }
}

pub(crate) async fn exchange(
    connection: &Connection,
    url: &str,
    body: &Value,
    mut text: impl FnMut(&str),
) -> Result<Value, String> {
    let (url, body) = request(&connection.provider, url, body);
    let mut response = super::response(connection, &url, &body).await?;
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
    {
        return Err("Provider did not return a streaming response. Check model support.".into());
    }
    let mut decoder = Decoder::default();
    let mut stream = Accumulator::new(&connection.provider);
    let mut total = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Reply stream interrupted. Check your connection.")?
    {
        total += chunk.len();
        if total > MAX_STREAM_BYTES {
            return Err("Reply stream exceeded the size limit.".into());
        }
        for event in decoder.push(&chunk)? {
            if event == "[DONE]" {
                continue;
            }
            let data = serde_json::from_str(&event)
                .map_err(|_| "Provider sent invalid streaming data.")?;
            if let Some(delta) = stream.push(data)? {
                text(&delta);
            }
            if stream.done && connection.provider != "gemini" {
                return stream.finish();
            }
        }
    }
    if !decoder.buffer.iter().all(u8::is_ascii_whitespace) {
        return Err("Reply stream ended in the middle of an event.".into());
    }
    stream.finish()
}

#[derive(Default)]
struct Decoder {
    buffer: Vec<u8>,
}
impl Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = vec![];
        loop {
            let boundary = self
                .buffer
                .windows(2)
                .position(|v| v == b"\n\n")
                .map(|p| (p, 2))
                .into_iter()
                .chain(
                    self.buffer
                        .windows(4)
                        .position(|v| v == b"\r\n\r\n")
                        .map(|p| (p, 4)),
                )
                .min_by_key(|(p, _)| *p);
            let Some((end, separator)) = boundary else {
                break;
            };
            if end > MAX_EVENT_BYTES {
                return Err("Provider stream event exceeds the size limit.".into());
            }
            let frame = self.buffer.drain(..end + separator).collect::<Vec<_>>();
            let frame = std::str::from_utf8(&frame[..end])
                .map_err(|_| "Invalid UTF-8 in provider stream.")?;
            let lines: Vec<_> = frame
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|v| v.strip_prefix(' ').unwrap_or(v))
                })
                .collect();
            if !lines.is_empty() {
                events.push(lines.join("\n"));
            }
        }
        if self.buffer.len() > MAX_EVENT_BYTES {
            return Err("Provider stream event exceeds the size limit.".into());
        }
        Ok(events)
    }
}

struct Block {
    value: Value,
    input: String,
    stopped: bool,
}
struct Accumulator {
    provider: String,
    response: Value,
    blocks: BTreeMap<usize, Block>,
    done: bool,
}
impl Accumulator {
    fn new(provider: &str) -> Self {
        Self {
            provider: provider.into(),
            response: json!({}),
            blocks: BTreeMap::new(),
            done: false,
        }
    }
    fn push(&mut self, data: Value) -> Result<Option<String>, String> {
        if data.get("error").is_some()
            || matches!(data["type"].as_str(), Some("error" | "response.failed"))
        {
            return Err("Provider reported an error while streaming. Try again.".into());
        }
        match self.provider.as_str() {
            "openai" | "azure_openai" => match data["type"].as_str() {
                Some("response.output_text.delta") => Ok(data["delta"].as_str().map(str::to_owned)),
                Some("response.completed" | "response.incomplete") => {
                    self.response = data["response"].clone();
                    self.done = true;
                    Ok(None)
                }
                _ => Ok(None),
            },
            "claude" => self.claude(data),
            _ => self.gemini(data),
        }
    }
    fn claude(&mut self, data: Value) -> Result<Option<String>, String> {
        match data["type"].as_str() {
            Some("message_start") => self.response = data["message"].clone(),
            Some("content_block_start") => {
                let index = index(&data)?;
                if self.blocks.contains_key(&index) {
                    return Err("Provider repeated a content block.".into());
                }
                let value = data["content_block"].clone();
                let text = (value["type"] == "text")
                    .then(|| value["text"].as_str().unwrap_or_default().to_owned());
                self.blocks.insert(
                    index,
                    Block {
                        value,
                        input: String::new(),
                        stopped: false,
                    },
                );
                return Ok(text);
            }
            Some("content_block_delta") => {
                let block = self
                    .blocks
                    .get_mut(&index(&data)?)
                    .filter(|b| !b.stopped)
                    .ok_or("Invalid streamed content block")?;
                let delta = &data["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") if block.value["type"] == "text" => {
                        let text = delta["text"].as_str().ok_or("Invalid text delta")?;
                        append(&mut block.value, "text", text);
                        return Ok(Some(text.into()));
                    }
                    Some("input_json_delta") => block.input.push_str(
                        delta["partial_json"]
                            .as_str()
                            .ok_or("Invalid tool input delta")?,
                    ),
                    Some("thinking_delta") => append(
                        &mut block.value,
                        "thinking",
                        delta["thinking"].as_str().unwrap_or_default(),
                    ),
                    Some("signature_delta") => append(
                        &mut block.value,
                        "signature",
                        delta["signature"].as_str().unwrap_or_default(),
                    ),
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let block = self
                    .blocks
                    .get_mut(&index(&data)?)
                    .ok_or("Invalid streamed content block")?;
                if !block.input.is_empty() {
                    block.value["input"] = serde_json::from_str(&block.input)
                        .map_err(|_| "Provider returned incomplete tool arguments")?;
                }
                block.stopped = true;
            }
            Some("message_delta") => {
                if let Some(delta) = data["delta"].as_object() {
                    for (key, value) in delta {
                        self.response[key] = value.clone();
                    }
                }
                if let Some(usage) = data["usage"].as_object() {
                    for (key, value) in usage {
                        self.response["usage"][key] = value.clone();
                    }
                }
            }
            Some("message_stop") => {
                if self.blocks.values().any(|b| !b.stopped) {
                    return Err("Provider stopped before completing its content blocks.".into());
                }
                self.response["content"] =
                    json!(self.blocks.values().map(|b| &b.value).collect::<Vec<_>>());
                self.done = true;
            }
            _ => {}
        }
        Ok(None)
    }
    fn gemini(&mut self, data: Value) -> Result<Option<String>, String> {
        if data
            .get("promptFeedback")
            .is_some_and(|v| v.get("blockReason").is_some())
        {
            return Err("Gemini blocked this request.".into());
        }
        let candidate = data["candidates"]
            .as_array()
            .and_then(|items| items.iter().find(|c| c["index"].as_u64().unwrap_or(0) == 0));
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        if self.response["candidates"].is_null() {
            self.response = json!({"candidates":[{"content":{"role":"model","parts":[]}}]});
        }
        let content = &mut self.response["candidates"][0];
        let mut text = String::new();
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            let accumulated = content["content"]["parts"].as_array_mut().unwrap();
            for part in parts {
                if part["thought"] != true {
                    if let Some(delta) = part["text"].as_str() {
                        text.push_str(delta);
                    }
                }
                let previous = accumulated.last_mut();
                if let Some(previous) = previous.filter(|p| {
                    p["text"].is_string()
                        && part["text"].is_string()
                        && p["thought"] == part["thought"]
                        && p.get("thoughtSignature").is_none()
                }) {
                    append(previous, "text", part["text"].as_str().unwrap());
                    if let Some(signature) = part.get("thoughtSignature") {
                        previous["thoughtSignature"] = signature.clone();
                    }
                } else {
                    accumulated.push(part.clone());
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason") {
            content["finishReason"] = reason.clone();
            self.done = true;
        }
        Ok((!text.is_empty()).then_some(text))
    }
    fn finish(self) -> Result<Value, String> {
        if !self.done {
            return Err("Reply stream ended before completion. Try again.".into());
        }
        if self.response["status"] == "incomplete"
            || self.response["stop_reason"] == "max_tokens"
            || self.response["candidates"][0]["finishReason"] == "MAX_TOKENS"
        {
            return Err(
                "Reply reached the provider output limit. Ask for a shorter answer.".into(),
            );
        }
        if self.provider == "gemini" && self.response["candidates"][0]["finishReason"] != "STOP" {
            return Err("Gemini stopped the reply before completion.".into());
        }
        providers::calls(&self.provider, &self.response)?;
        Ok(self.response)
    }
}
fn index(data: &Value) -> Result<usize, String> {
    let index = data["index"].as_u64().ok_or("Invalid stream block index")?;
    if index > 100 {
        return Err("Provider sent too many content blocks.".into());
    }
    Ok(index as usize)
}
fn append(value: &mut Value, key: &str, delta: &str) {
    let text = format!("{}{delta}", value[key].as_str().unwrap_or_default());
    value[key] = json!(text);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_decodes_every_byte_boundary_unicode_multiline_data_and_heartbeats() {
        let raw = ": heartbeat\r\n\r\nevent: text\r\ndata: {\r\ndata: \"text\":\"café 📝\"}\r\n\r\ndata: [DONE]\n\n";
        for size in 1..raw.len() {
            let mut decoder = Decoder::default();
            let mut events = vec![];
            for bytes in raw.as_bytes().chunks(size) {
                events.extend(decoder.push(bytes).unwrap());
            }
            assert_eq!(events, vec!["{\n\"text\":\"café 📝\"}", "[DONE]"]);
            assert!(decoder.buffer.is_empty());
        }
        assert!(Decoder::default()
            .push(&vec![b'x'; MAX_EVENT_BYTES + 1])
            .is_err());
        assert!(Decoder::default().push(b"data: \xff\n\n").is_err());
    }

    #[test]
    fn responses_streams_only_visible_text_and_retains_complete_tool_and_reasoning_items() {
        for provider in ["openai", "azure_openai"] {
            let mut stream = Accumulator::new(provider);
            assert!(stream
                .push(json!({"type":"response.reasoning_text.delta","delta":"private"}))
                .unwrap()
                .is_none());
            assert!(stream.push(json!({"type":"response.function_call_arguments.delta","delta":"{\"filename\":"})).unwrap().is_none());
            assert_eq!(
                stream
                    .push(json!({"type":"response.output_text.delta","delta":"Checking…"}))
                    .unwrap(),
                Some("Checking…".into())
            );
            let final_response = json!({"status":"completed","output":[{"type":"reasoning","id":"reason","encrypted_content":"opaque"},{"type":"function_call","call_id":"call","name":"read_entry","arguments":"{\"filename\":\"a.md\"}"}]});
            stream
                .push(json!({"type":"response.completed","response":final_response}))
                .unwrap();
            let result = stream.finish().unwrap();
            assert_eq!(result, final_response);
            assert_eq!(
                providers::calls(provider, &result).unwrap()[0].arguments["filename"],
                "a.md"
            );
            let (_, mut body) = providers::request(
                &Connection {
                    provider: provider.into(),
                    model: "test".into(),
                    endpoint: "https://resource.openai.azure.com".into(),
                    ..Default::default()
                },
                "purpose",
                &[],
            )
            .unwrap();
            providers::append_results(
                provider,
                &mut body,
                &result,
                &[(
                    providers::calls(provider, &result).unwrap()[0].clone(),
                    json!({"body":"page"}),
                )],
            )
            .unwrap();
            assert!(body.to_string().contains("opaque"));
            assert!(body.to_string().contains("call"));
        }
    }

    #[test]
    fn claude_collects_text_tool_input_and_thinking_signatures_without_displaying_them() {
        let mut stream = Accumulator::new("claude");
        stream.push(json!({"type":"message_start","message":{"role":"assistant","content":[],"usage":{"input_tokens":10}}})).unwrap();
        let events = [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello "}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"there"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call","name":"read_entry","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"filename\":\"a"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":".md\"}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}),
            json!({"type":"message_stop"}),
        ];
        let mut visible = String::new();
        for event in events {
            if let Some(delta) = stream.push(event).unwrap() {
                visible.push_str(&delta);
            }
        }
        assert_eq!(visible, "Hello there");
        let result = stream.finish().unwrap();
        assert_eq!(result["content"][0]["signature"], "opaque");
        assert_eq!(result["usage"]["input_tokens"], 10);
        assert_eq!(result["usage"]["output_tokens"], 20);
        assert_eq!(
            providers::calls("claude", &result).unwrap()[0].arguments["filename"],
            "a.md"
        );
        assert_eq!(providers::text("claude", &result).unwrap(), visible);
    }

    #[test]
    fn gemini_merges_streamed_text_and_preserves_signed_function_parts() {
        let mut stream = Accumulator::new("gemini");
        let mut text = String::new();
        for part in [
            json!({"text":"private","thought":true}),
            json!({"text":"Hello "}),
            json!({"text":"world","thoughtSignature":"text-signature"}),
            json!({"functionCall":{"id":"call","name":"read_entry","args":{"filename":"a.md"}},"thoughtSignature":"opaque"}),
        ] {
            if let Some(delta) = stream
                .push(json!({"candidates":[{"index":0,"content":{"parts":[part]}}]}))
                .unwrap()
            {
                text.push_str(&delta);
            }
        }
        stream
            .push(json!({"candidates":[{"index":0,"finishReason":"STOP"}]}))
            .unwrap();
        let result = stream.finish().unwrap();
        assert_eq!(text, "Hello world");
        assert_eq!(providers::text("gemini", &result).unwrap(), text);
        assert_eq!(providers::calls("gemini", &result).unwrap()[0].id, "call");
        assert_eq!(
            result["candidates"][0]["content"]["parts"][1]["thoughtSignature"],
            "text-signature"
        );
        assert_eq!(
            result["candidates"][0]["content"]["parts"][2]["thoughtSignature"],
            "opaque"
        );
    }

    #[test]
    fn incomplete_failed_and_malformed_tool_streams_cannot_finish_successfully() {
        for provider in ["openai", "azure_openai", "claude", "gemini"] {
            assert!(Accumulator::new(provider).finish().is_err());
            assert!(Accumulator::new(provider)
                .push(json!({"error":{"message":"secret"}}))
                .unwrap_err()
                .find("secret")
                .is_none());
        }
        let mut stream = Accumulator::new("openai");
        stream.push(json!({"type":"response.incomplete","response":{"status":"incomplete","output":[{"type":"function_call","name":"delete_entry","call_id":"call","arguments":"{}"}]}})).unwrap();
        assert!(stream.finish().is_err());
        let mut stream = Accumulator::new("openai");
        stream.push(json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","name":"delete_entry","call_id":"call","arguments":"{"}]}})).unwrap();
        assert!(stream.finish().is_err());
        let mut stream = Accumulator::new("claude");
        stream
            .push(json!({"type":"message_start","message":{"content":[]}}))
            .unwrap();
        stream.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"delete_entry","input":{}}})).unwrap();
        assert!(stream.push(json!({"type":"message_stop"})).is_err());
        let mut stream = Accumulator::new("gemini");
        stream
            .push(json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}))
            .unwrap();
        assert!(stream.finish().is_err());
    }

    #[test]
    fn http_delivers_delta_before_final_response_and_uses_stream_request() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::mpsc,
            time::Duration,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/responses", listener.local_addr().unwrap());
        let (sent, received) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            let mut byte = [0];
            while !bytes.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
            }
            let headers = String::from_utf8(bytes).unwrap().to_lowercase();
            assert!(headers.contains("authorization: bearer test-key"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut raw = vec![0; length];
            socket.read_exact(&mut raw).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&raw).unwrap()["stream"],
                true
            );
            let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"café\"}\n\n";
            let final_response = json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"café"}]}]});
            let final_event = format!(
                "data: {}\n\n",
                json!({"type":"response.completed","response":final_response})
            );
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{delta}",delta.len()+final_event.len()).unwrap();
            socket.flush().unwrap();
            received
                .recv_timeout(Duration::from_secs(5))
                .expect("Delta must arrive before final response is sent");
            socket.write_all(final_event.as_bytes()).unwrap();
        });
        tauri::async_runtime::block_on(async {
            let mut text = String::new();
            let result = exchange(
                &Connection {
                    provider: "openai".into(),
                    api_key: "test-key".into(),
                    ..Default::default()
                },
                &url,
                &json!({"input":"hello"}),
                |delta| {
                    text.push_str(delta);
                    sent.send(()).unwrap();
                },
            )
            .await
            .unwrap();
            assert_eq!(text, "café");
            assert_eq!(providers::text("openai", &result).unwrap(), text);
        });
        server.join().unwrap();
        let (url, body) = request(
            "gemini",
            "https://example.com/model:generateContent",
            &json!({"contents":[]}),
        );
        assert!(url.ends_with(":streamGenerateContent?alt=sse"));
        assert!(body.get("stream").is_none());
    }
}
