use super::{Connection, Message};
use serde_json::{json, Value};

pub(crate) fn validate(provider: &str, model: &str) -> Result<(), String> {
    if !["openai", "azure_openai", "claude", "gemini"].contains(&provider) {
        return Err("Choose OpenAI, Azure OpenAI, Claude, or Gemini".into());
    }
    if model.is_empty()
        || model.len() > 100
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("Enter a valid model ID".into());
    }
    Ok(())
}

pub(crate) fn validate_connection(c: &Connection) -> Result<(), String> {
    validate(&c.provider, &c.model)?;
    if c.provider == "azure_openai" {
        azure_url(&c.endpoint)?;
    }
    Ok(())
}

pub(crate) fn http_error(c: &Connection, code: u16) -> String {
    match code {
        401 | 403 => "Provider rejected the API key or model access.".into(),
        429 => "Provider rate limit or quota reached. Try again later.".into(),
        500 | 502 | 503 | 504 => {
            format!("Provider is temporarily unavailable (HTTP {code}). Try again shortly.")
        }
        404 if c.provider == "gemini" => format!(
            "Gemini model {} is unavailable for this key or API (HTTP 404). Use /model available to find models, then /model <model> to switch. Listed models may still be unavailable to your account.",
            c.model
        ),
        _ => format!("Provider request failed (HTTP {code}). Check the model name and account."),
    }
}
fn azure_url(endpoint: &str) -> Result<String, String> {
    let url =
        reqwest::Url::parse(endpoint).map_err(|_| "Enter your Azure HTTPS resource endpoint")?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || ![
            ".openai.azure.com",
            ".services.ai.azure.com",
            ".cognitiveservices.azure.com",
        ]
        .iter()
        .any(|suffix| host.ends_with(suffix))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
        || !["", "/", "/openai/v1", "/openai/v1/"].contains(&url.path())
    {
        return Err("Use your Azure resource HTTPS endpoint, optionally ending in /openai/v1/. Credentials belong in the API key field.".into());
    }
    Ok(format!("https://{host}/openai/v1/responses"))
}

pub(crate) fn request(
    c: &Connection,
    system: &str,
    messages: &[Message],
) -> Result<(String, Value), String> {
    validate_connection(c)?;
    let plain: Vec<_> = messages.iter().map(|m|json!({"role":m.role,"content":format!("{}{}",m.content, if m.proposals.is_empty(){String::new()}else{format!("\nTool proposals and current status: {}",serde_json::to_string(&m.proposals.iter().map(|p|json!({"id":p.id,"tool":p.name,"applied":p.applied,"result":p.arguments.get("result")})).collect::<Vec<_>>()).unwrap())})})).collect();
    Ok(match c.provider.as_str() {
        "openai" | "azure_openai" => (
            if c.provider == "azure_openai" {
                azure_url(&c.endpoint)?
            } else {
                "https://api.openai.com/v1/responses".into()
            },
            json!({"model":c.model,"instructions":system,"input":plain,"max_output_tokens":4096,"store":false}),
        ),
        "claude" => (
            "https://api.anthropic.com/v1/messages".into(),
            json!({"model":c.model,"system":system,"messages":plain,"max_tokens":4096}),
        ),
        _ => (
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                c.model
            ),
            json!({"systemInstruction":{"parts":[{"text":system}]},"contents":plain.iter().map(|m| json!({"role":if m["role"]=="assistant" {"model"} else {"user"},"parts":[{"text":m["content"]}]})).collect::<Vec<_>>(),"generationConfig":{"maxOutputTokens":4096}}),
        ),
    })
}

pub(crate) fn text(provider: &str, data: &Value) -> Result<String, String> {
    if data.get("status").and_then(Value::as_str) == Some("incomplete") {
        return Err(
            "Response exceeded the provider output limit. Ask for a shorter answer.".into(),
        );
    }
    let blocks: Vec<&Value> = match provider {
        "openai" | "azure_openai" => data["output"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter(|b| b["type"] == "output_text")
            .collect(),
        "claude" => {
            if data["stop_reason"] == "max_tokens" {
                return Err("Response exceeded the output limit. Ask for a shorter answer.".into());
            }
            data["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["type"] == "text")
                .collect()
        }
        _ => {
            if data["candidates"][0]["finishReason"] == "MAX_TOKENS" {
                return Err("Response exceeded the output limit. Ask for a shorter answer.".into());
            }
            data["candidates"][0]["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["thought"] != true)
                .collect()
        }
    };
    let text = blocks
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        Err("Provider returned no text. Try a different request or model.".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_requests_keep_system_and_roles_separate() {
        for provider in ["openai", "claude", "gemini"] {
            let c = Connection {
                provider: provider.into(),
                model: "test-model".into(),
                models: vec![],
                api_key: "secret".into(),
                endpoint: String::new(),
            };
            let messages = vec![
                Message {
                    proposals: vec![],
                    role: "user".into(),
                    content: "question".into(),
                },
                Message {
                    proposals: vec![],
                    role: "assistant".into(),
                    content: "reply".into(),
                },
            ];
            let (url, body) = request(&c, "purpose", &messages).unwrap();
            assert!(url.starts_with("https://"));
            assert!(!body.to_string().contains("secret"));
            if provider == "gemini" {
                assert_eq!(body["contents"][1]["role"], "model");
                assert_eq!(body["systemInstruction"]["parts"][0]["text"], "purpose");
            }
            if matches!(provider, "openai" | "azure_openai") {
                assert_eq!(body["store"], false);
                assert_eq!(body["instructions"], "purpose");
            }
            if provider == "claude" {
                assert_eq!(body["system"], "purpose");
            }
        }
    }
    #[test]
    fn parses_provider_text_and_ignores_reasoning() {
        assert_eq!(
            text(
                "openai",
                &json!({"output":[{"content":[{"type":"output_text","text":"page"}]}]})
            )
            .unwrap(),
            "page"
        );
        assert_eq!(text("claude", &json!({"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"page"}]})).unwrap(), "page");
        assert_eq!(text("gemini", &json!({"candidates":[{"content":{"parts":[{"thought":true,"text":"private"},{"text":"page"}]}}]})).unwrap(), "page");
        assert!(text("openai", &json!({"status":"incomplete"})).is_err());
        assert!(text("gemini", &json!({"candidates":[]})).is_err());
    }
    #[test]
    fn azure_responses_support_tools_and_validate_resource_endpoint() {
        let c = Connection {
            provider: "azure_openai".into(),
            model: "my-deployment".into(),
            models: vec![],
            api_key: "secret".into(),
            endpoint: "https://my-resource.openai.azure.com/openai/v1/".into(),
        };
        let (url, mut body) = request(&c, "purpose", &[]).unwrap();
        assert_eq!(
            url,
            "https://my-resource.openai.azure.com/openai/v1/responses"
        );
        assert_eq!(body["model"], "my-deployment");
        attach_tools("azure_openai", &mut body, &crate::ai::tools::definitions());
        assert_eq!(body["tools"][0]["type"], "function");
        let data = json!({"output":[{"type":"function_call","call_id":"call1","name":"read_entry","arguments":"{}"},{"type":"message","content":[{"type":"output_text","text":"Done"}]}]});
        let calls = calls("azure_openai", &data).unwrap();
        assert_eq!(calls[0].id, "call1");
        append_results(
            "azure_openai",
            &mut body,
            &data,
            &[(calls[0].clone(), json!({"ok":true}))],
        )
        .unwrap();
        assert_eq!(
            body["input"].as_array().unwrap().last().unwrap()["type"],
            "function_call_output"
        );
        assert_eq!(text("azure_openai", &data).unwrap(), "Done");
        assert!(!body.to_string().contains("secret"));
        for endpoint in [
            "http://r.openai.azure.com",
            "https://evil.example",
            "https://user:secret@r.openai.azure.com",
            "https://r.openai.azure.com/?key=secret",
            "https://r.openai.azure.com/other",
        ] {
            assert!(azure_url(endpoint).is_err());
        }
    }
    #[test]
    fn rejects_unknown_providers_and_model_path_injection() {
        assert!(validate("other", "model").is_err());
        assert!(validate("gemini", "model?key=secret").is_err());
    }
    #[test]
    fn gemini_missing_models_explain_how_to_switch_without_exposing_credentials() {
        let connection = Connection {
            provider: "gemini".into(),
            model: "unavailable-model".into(),
            api_key: "private-key".into(),
            ..Default::default()
        };
        let error = http_error(&connection, 404);
        assert!(error.contains("unavailable-model"));
        assert!(error.contains("/model available"));
        assert!(error.contains("/model <model>"));
        assert!(!error.contains(&connection.api_key));
        assert!(http_error(&connection, 429).contains("quota"));
        assert!(http_error(&connection, 403).contains("API key"));
        assert!(http_error(&connection, 503).contains("temporarily unavailable"));
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

pub(crate) fn calls(provider: &str, data: &Value) -> Result<Vec<ToolCall>, String> {
    let items: Vec<&Value> = match provider {
        "openai" | "azure_openai" => data["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == "function_call")
            .collect(),
        "claude" => data["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == "tool_use")
            .collect(),
        _ => data["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.get("functionCall"))
            .collect(),
    };
    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            let name = v["name"]
                .as_str()
                .ok_or("Tool call is missing its name")?
                .to_owned();
            let arguments = match provider {
                "openai" | "azure_openai" => {
                    serde_json::from_str(v["arguments"].as_str().ok_or("Invalid tool arguments")?)
                        .map_err(|_| "Invalid tool argument JSON")?
                }
                "claude" => v["input"].clone(),
                _ => v["args"].clone(),
            };
            if !arguments.is_object() {
                return Err("Tool arguments must be an object".into());
            }
            let id = if matches!(provider, "openai" | "azure_openai") {
                v["call_id"].as_str()
            } else {
                v["id"].as_str()
            };
            Ok(ToolCall {
                id: id.map(str::to_owned).unwrap_or_else(|| format!("call-{i}")),
                name,
                arguments,
            })
        })
        .collect()
}

pub(crate) fn attach_tools(provider: &str, body: &mut Value, definitions: &[Value]) {
    body["tools"] = match provider {
        "openai" | "azure_openai" => json!(definitions.iter().map(|d| json!({"type":"function","name":d["name"],"description":d["description"],"parameters":d["parameters"],"strict":false})).collect::<Vec<_>>()),
        "claude" => json!(definitions.iter().map(|d| json!({"name":d["name"],"description":d["description"],"input_schema":d["parameters"]})).collect::<Vec<_>>()),
        _ => json!([{"functionDeclarations":definitions.iter().map(|d| json!({"name":d["name"],"description":d["description"],"parametersJsonSchema":d["parameters"]})).collect::<Vec<_>>()}]),
    };
    if matches!(provider, "openai" | "azure_openai") {
        body["parallel_tool_calls"] = json!(false);
    }
}

pub(crate) fn append_results(
    provider: &str,
    body: &mut Value,
    data: &Value,
    results: &[(ToolCall, Value)],
) -> Result<(), String> {
    let key = match provider {
        "openai" | "azure_openai" => "input",
        "claude" => "messages",
        _ => "contents",
    };
    let conversation = body[key]
        .as_array_mut()
        .ok_or("Invalid provider conversation")?;
    match provider {
        "openai" | "azure_openai" => {
            conversation.extend(
                data["output"]
                    .as_array()
                    .ok_or("Invalid OpenAI output")?
                    .clone(),
            );
            for (call, result) in results {
                conversation.push(json!({"type":"function_call_output","call_id":call.id,"output":result.to_string()}));
            }
        }
        "claude" => {
            conversation.push(json!({"role":"assistant","content":data["content"]}));
            conversation.push(json!({"role":"user","content":results.iter().map(|(call,result)|json!({"type":"tool_result","tool_use_id":call.id,"content":result.to_string()})).collect::<Vec<_>>()}));
        }
        _ => {
            // Preserve the complete model content, including Gemini thought signatures.
            conversation.push(data["candidates"][0]["content"].clone());
            conversation.push(
                json!({"role":"user","parts":results.iter().map(|(call,result)| {
                let mut response = json!({"name":call.name,"response":result});
                if !call.id.starts_with("call-") { response["id"] = json!(call.id); }
                json!({"functionResponse":response})
            }).collect::<Vec<_>>()}),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    #[test]
    fn gemini_accepts_json_schemas_for_notebook_editor_and_external_tools() {
        let mut definitions = crate::ai::tools::definitions();
        definitions.extend(crate::ai::editor::definitions());
        definitions.push(json!({
            "name": "mcp_nested",
            "description": "External tool with a full JSON schema",
            "parameters": {
                "type": "object",
                "properties": {"options": {"type": "object", "additionalProperties": {"type": "string"}}},
                "additionalProperties": false
            }
        }));
        let mut body = json!({});
        attach_tools("gemini", &mut body, &definitions);
        let declarations = body["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), definitions.len());
        for (declaration, definition) in declarations.iter().zip(&definitions) {
            assert!(declaration.get("parameters").is_none());
            assert_eq!(
                declaration["parametersJsonSchema"],
                definition["parameters"]
            );
            assert_eq!(declaration["name"], definition["name"]);
        }
    }
    #[test]
    fn each_provider_pairs_tool_calls_and_results() {
        let responses = [
            (
                "openai",
                json!({"output":[{"type":"function_call","call_id":"abc","name":"read_entry","arguments":"{\"filename\":\"a.md\"}"}]}),
            ),
            (
                "claude",
                json!({"content":[{"type":"tool_use","id":"abc","name":"read_entry","input":{"filename":"a.md"}}]}),
            ),
            (
                "gemini",
                json!({"candidates":[{"content":{"role":"model","parts":[{"thoughtSignature":"signature","functionCall":{"id":"abc","name":"read_entry","args":{"filename":"a.md"}}}]}}]}),
            ),
        ];
        for (provider, data) in responses {
            let c = Connection {
                provider: provider.into(),
                model: "model".into(),
                models: vec![],
                api_key: "secret".into(),
                endpoint: String::new(),
            };
            let (_, mut body) = request(
                &c,
                "purpose",
                &[Message {
                    role: "user".into(),
                    content: "Read page".into(),
                    proposals: vec![],
                }],
            )
            .unwrap();
            attach_tools(provider, &mut body, &crate::ai::tools::definitions());
            let calls = calls(provider, &data).unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].arguments["filename"], "a.md");
            append_results(
                provider,
                &mut body,
                &data,
                &[(calls[0].clone(), json!({"body":"prose"}))],
            )
            .unwrap();
            let serialized = body.to_string();
            assert!(serialized.contains("abc"));
            assert!(serialized.contains("prose"));
            if provider == "gemini" {
                assert!(serialized.contains("signature"));
            }
            assert!(!serialized.contains("secret"));
        }
    }
    #[test]
    fn malformed_arguments_fail_before_tool_execution() {
        assert!(calls(
            "openai",
            &json!({"output":[{"type":"function_call","name":"read_entry","arguments":"not json"}]})
        )
        .is_err());
    }
}
