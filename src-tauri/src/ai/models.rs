use super::Connection;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Serialize)]
pub(crate) struct ModelList {
    pub models: Vec<String>,
    pub note: String,
}
fn parse(provider: &str, data: &Value) -> Result<(Vec<String>, Option<String>), String> {
    let items = data[if provider == "gemini" {
        "models"
    } else {
        "data"
    }]
    .as_array()
    .ok_or("Provider returned an invalid model list")?;
    let mut models = vec![];
    for item in items {
        if provider == "gemini"
            && !item["supportedGenerationMethods"]
                .as_array()
                .is_some_and(|methods| methods.iter().any(|m| m == "generateContent"))
        {
            continue;
        }
        let id = item[if provider == "gemini" { "name" } else { "id" }]
            .as_str()
            .ok_or("Invalid model ID in provider list")?
            .trim_start_matches("models/");
        if super::providers::validate(provider, id).is_ok() {
            models.push(id.into());
        }
    }
    let next = if provider == "gemini" {
        data["nextPageToken"].as_str().map(str::to_owned)
    } else if provider == "claude" && data["has_more"] == true {
        Some(
            data["last_id"]
                .as_str()
                .ok_or("Missing model pagination cursor")?
                .into(),
        )
    } else {
        None
    };
    Ok((models, next))
}
pub(crate) async fn list(connection: &Connection) -> Result<ModelList, String> {
    super::providers::validate_connection(connection)?;
    if connection.provider == "azure_openai" {
        return Ok(ModelList {models:connection.models.iter().cloned().chain(std::iter::once(connection.model.clone())).collect(),note:"Azure uses deployment names. These are your saved deployments; use /model <deployment> to choose another Responses-compatible deployment from your Azure resource.".into()});
    }
    if connection.api_key.is_empty() {
        return Err("Connect this provider first".into());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Cannot initialize model discovery")?;
    let base = match connection.provider.as_str() {
        "openai" => "https://api.openai.com/v1/models",
        "claude" => "https://api.anthropic.com/v1/models",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/models",
        _ => return Err("Unsupported provider".into()),
    };
    let mut models = vec![];
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let mut request = client.get(base);
        request = match connection.provider.as_str() {
            "openai" => request.bearer_auth(&connection.api_key),
            "claude" => request
                .header("x-api-key", &connection.api_key)
                .header("anthropic-version", "2023-06-01")
                .query(&[("limit", "1000")]),
            _ => request
                .header("x-goog-api-key", &connection.api_key)
                .query(&[("pageSize", "1000")]),
        };
        if let Some(cursor) = &cursor {
            request = request.query(&[(
                if connection.provider == "gemini" {
                    "pageToken"
                } else {
                    "after_id"
                },
                cursor,
            )]);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "Cannot connect to provider model list")?;
        if !response.status().is_success() {
            return Err(format!(
                "Model discovery failed (HTTP {}). Check your connection and model permissions.",
                response.status().as_u16()
            ));
        }
        let mut bytes = vec![];
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Cannot read model list")?
        {
            if bytes.len() + chunk.len() > 512_000 {
                return Err("Model list exceeds size limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let data = serde_json::from_slice(&bytes).map_err(|_| "Invalid model list response")?;
        let (page, next) = parse(&connection.provider, &data)?;
        models.extend(page);
        models.sort();
        models.dedup();
        if models.len() > 2000 {
            return Err("Too many provider models".into());
        }
        if next.is_none() {
            if !models.contains(&connection.model) {
                models.insert(0, connection.model.clone());
            }
            return Ok(ModelList {models,note:"Models reported by your provider, plus your saved model. Choose a model that supports text responses and tools.".into()});
        }
        if next == cursor {
            return Err("Provider repeated model pagination cursor".into());
        }
        cursor = next;
    }
    Err("Model discovery exceeded pagination limit".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn catalogs_filter_non_generation_models_and_keep_pagination() {
        let (models,next)=parse("gemini",&json!({"models":[{"name":"models/gemini-example","supportedGenerationMethods":["generateContent"]},{"name":"models/embedding","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"next"})).unwrap();
        assert_eq!(models, vec!["gemini-example"]);
        assert_eq!(next.as_deref(), Some("next"));
        let (models, next) = parse(
            "claude",
            &json!({"data":[{"id":"claude-example"}],"has_more":true,"last_id":"claude-example"}),
        )
        .unwrap();
        assert_eq!(models, vec!["claude-example"]);
        assert!(next.is_some());
        assert!(parse("openai", &json!({"error":"private"})).is_err());
    }
}
