use super::{path, Message};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tauri::AppHandle;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
#[derive(Serialize, Deserialize)]
struct Conversation {
    id: String,
    title: String,
    updated_at: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    messages: Vec<Message>,
}
#[derive(Serialize, Deserialize)]
struct Store {
    version: u32,
    active: String,
    conversations: Vec<Conversation>,
}
#[derive(Serialize)]
pub(crate) struct Summary {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub messages: usize,
    pub active: bool,
}
fn conversation(id: String, messages: Vec<Message>) -> Conversation {
    let title = messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| {
            m.content
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect()
        })
        .unwrap_or_else(|| "New conversation".into());
    Conversation {
        id,
        title,
        updated_at: chrono::Utc::now().to_rfc3339(),
        provider: None,
        model: None,
        messages,
    }
}
fn new_id() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}
fn location(app: &AppHandle, notebook: &PathBuf) -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    notebook.hash(&mut hash);
    path(app, &format!("chat-{:x}.json", hash.finish()))
}
fn load(path: &Path) -> Result<Store, String> {
    if !path.exists() {
        return Ok(Store {
            version: 1,
            active: "legacy".into(),
            conversations: vec![conversation("legacy".into(), vec![])],
        });
    }
    let bytes = fs::read(path).map_err(|_| "Cannot read chat history")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Chat history is invalid")?;
    let store: Store = if value.is_array() {
        Store {
            version: 1,
            active: "legacy".into(),
            conversations: vec![conversation(
                "legacy".into(),
                serde_json::from_value(value).map_err(|_| "Chat history is invalid")?,
            )],
        }
    } else {
        serde_json::from_value(value).map_err(|_| "Chat history is invalid")?
    };
    if store.version != 1 || !store.conversations.iter().any(|c| c.id == store.active) {
        return Err("Chat history is invalid".into());
    }
    Ok(store)
}
fn persist(path: &Path, store: &Store) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        if temporary.exists() {
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
                .map_err(|_| "Cannot protect chat history")?;
        }
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| "Cannot save chat history")?;
    file.write_all(&serde_json::to_vec(store).map_err(|_| "Cannot serialize chat history")?)
        .and_then(|_| file.sync_all())
        .map_err(|_| "Cannot save chat history")?;
    fs::rename(temporary, path).map_err(|_| "Cannot replace chat history".into())
}
pub(crate) fn messages(app: &AppHandle, notebook: &PathBuf) -> Result<Vec<Message>, String> {
    let store = load(&location(app, notebook)?)?;
    Ok(store
        .conversations
        .into_iter()
        .find(|c| c.id == store.active)
        .unwrap()
        .messages)
}
pub(crate) fn save(
    app: &AppHandle,
    notebook: &PathBuf,
    messages: &[Message],
    connection: Option<(&str, &str)>,
) -> Result<(), String> {
    let path = location(app, notebook)?;
    let mut store = load(&path)?;
    let active = store
        .conversations
        .iter_mut()
        .find(|c| c.id == store.active)
        .unwrap();
    active.messages = messages.to_vec();
    active.title = conversation(active.id.clone(), messages.to_vec()).title;
    active.updated_at = chrono::Utc::now().to_rfc3339();
    if let Some((provider, model)) = connection {
        active.provider = Some(provider.into());
        active.model = Some(model.into());
    }
    persist(&path, &store)
}
fn start(store: &mut Store) {
    // Reuse an empty conversation instead of filling history with blank sessions.
    if store
        .conversations
        .iter()
        .any(|c| c.id == store.active && c.messages.is_empty())
    {
        return;
    }
    let id = new_id();
    store.conversations.push(conversation(id.clone(), vec![]));
    store.active = id;
}
pub(crate) fn start_new(app: &AppHandle, notebook: &PathBuf) -> Result<(), String> {
    let path = location(app, notebook)?;
    let mut store = load(&path)?;
    start(&mut store);
    persist(&path, &store)
}
pub(crate) fn list(app: &AppHandle, notebook: &PathBuf) -> Result<Vec<Summary>, String> {
    let store = load(&location(app, notebook)?)?;
    let mut summaries: Vec<_> = store
        .conversations
        .into_iter()
        .filter(|c| !c.messages.is_empty())
        .map(|c| Summary {
            id: c.id.clone(),
            title: c.title,
            updated_at: c.updated_at,
            provider: c.provider,
            model: c.model,
            messages: c.messages.len(),
            active: c.id == store.active,
        })
        .collect();
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(summaries)
}
fn select(store: &mut Store, id: &str) -> Result<(), String> {
    if !store
        .conversations
        .iter()
        .any(|c| c.id == id && !c.messages.is_empty())
    {
        return Err("Conversation not found in this notebook".into());
    }
    store.active = id.into();
    Ok(())
}
pub(crate) fn resume(app: &AppHandle, notebook: &PathBuf, id: &str) -> Result<(), String> {
    let path = location(app, notebook)?;
    let mut store = load(&path)?;
    select(&mut store, id)?;
    persist(&path, &store)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn legacy_chats_survive_new_conversations_and_replay_status_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("ff-sessions-{}-{}", std::process::id(), new_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.json");
        fs::write(&path,json!([{"role":"user","content":"First chat"},{"role":"assistant","content":"Reply","proposals":[{"id":"p","name":"clear_editor","arguments":{},"before":[],"applied":true}]}]).to_string()).unwrap();
        let mut store = load(&path).unwrap();
        assert_eq!(store.active, "legacy");
        start(&mut store);
        assert_eq!(store.conversations.len(), 2);
        assert_ne!(store.active, "legacy");
        start(&mut store);
        assert_eq!(store.conversations.len(), 2);
        persist(&path, &store).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.conversations[0].title, "First chat");
        assert!(loaded.conversations[0].messages[1].proposals[0].applied);
        assert_eq!(loaded.conversations[0].messages[0].content, "First chat");
        let mut loaded = loaded;
        assert!(select(&mut loaded, "missing").is_err());
        assert!(select(&mut loaded, "legacy").is_ok());
        assert_eq!(loaded.active, "legacy");
        persist(&path, &loaded).unwrap();
        assert_eq!(load(&path).unwrap().active, "legacy");
        fs::remove_dir_all(dir).unwrap();
    }
}
