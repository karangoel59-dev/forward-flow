use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

// ---------------------------------------------------------------- config

#[derive(Serialize, Deserialize, Default, Clone)]
struct Config {
    vault: Option<String>,
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("config.json"))
}

fn read_config(app: &AppHandle) -> Config {
    config_path(app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn vault_dir(app: &AppHandle) -> Result<PathBuf, String> {
    read_config(app)
        .vault
        .map(PathBuf::from)
        .ok_or_else(|| "no vault selected".to_string())
}

// ---------------------------------------------------------------- entries

#[derive(Serialize, Clone)]
struct EntryMeta {
    path: String,
    name: String,
    created: String,
    words: usize,
    preview: String,
    tags: Vec<String>,
}

#[derive(Serialize)]
struct EntryFull {
    meta: EntryMeta,
    body: String,
}

/// Splits `---\n...\n---\n` frontmatter off the top of a file.
fn split_frontmatter(raw: &str) -> (Option<String>, String) {
    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return (
                Some(rest[..end].to_string()),
                rest[end + 5..].trim_start_matches('\n').to_string(),
            );
        }
    }
    (None, raw.to_string())
}

fn fm_value(fm: &str, key: &str) -> Option<String> {
    let needle = format!("{}:", key);
    fm.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&needle).map(|v| v.trim().to_string()))
}

fn parse_tags(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn meta_from(path: &PathBuf, raw: &str) -> EntryMeta {
    let (fm, body) = split_frontmatter(raw);
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let fm = fm.unwrap_or_default();
    let preview: String = body.split_whitespace().take(24).collect::<Vec<_>>().join(" ");

    EntryMeta {
        path: path.to_string_lossy().to_string(),
        created: fm_value(&fm, "created").unwrap_or_else(|| name.clone()),
        name,
        words: body.split_whitespace().count(),
        preview,
        tags: fm_value(&fm, "tags").map(|t| parse_tags(&t)).unwrap_or_default(),
    }
}

/// Rejects paths that resolve outside the vault.
fn entry_in_vault(app: &AppHandle, path: &str) -> Result<PathBuf, String> {
    let vault = vault_dir(app)?
        .canonicalize()
        .map_err(|e| format!("vault unreadable: {}", e))?;
    let target = PathBuf::from(path)
        .canonicalize()
        .map_err(|e| format!("entry unreadable: {}", e))?;
    if !target.starts_with(&vault) {
        return Err("entry is outside the vault".into());
    }
    Ok(target)
}

#[tauri::command]
fn get_vault(app: AppHandle) -> Option<String> {
    read_config(&app).vault
}

#[tauri::command]
fn set_vault(app: AppHandle, path: String) -> Result<(), String> {
    fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    let cfg = Config { vault: Some(path) };
    let body = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(&app)?, body).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_entries(app: AppHandle) -> Result<Vec<EntryMeta>, String> {
    let dir = vault_dir(&app)?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for item in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = match item {
            Ok(i) => i.path(),
            Err(_) => continue,
        };
        let is_md = path.extension().map(|e| e == "md").unwrap_or(false);
        let hidden = path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with('.'))
            .unwrap_or(true);
        if !is_md || hidden {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            out.push(meta_from(&path, &raw));
        }
    }
    // Newest first. Filenames are timestamps, so name order == time order.
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

#[tauri::command]
fn read_entry(app: AppHandle, path: String) -> Result<EntryFull, String> {
    let target = entry_in_vault(&app, &path)?;
    let raw = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let (_, body) = split_frontmatter(&raw);
    Ok(EntryFull {
        meta: meta_from(&target, &raw),
        body,
    })
}

/// Writes the draft to a new timestamped file and locks it. Never overwrites.
#[tauri::command]
fn commit_entry(app: AppHandle, content: String) -> Result<EntryMeta, String> {
    let dir = vault_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let body = content.trim();
    if body.is_empty() {
        return Err("nothing written yet".into());
    }

    let now = chrono::Local::now();
    let stamp = now.format("%Y-%m-%d-%H%M%S").to_string();
    let mut path = dir.join(format!("{}.md", stamp));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{}-{}.md", stamp, n));
        n += 1;
    }

    let raw = format!(
        "---\ncreated: {}\ntags: []\n---\n\n{}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        body
    );
    fs::write(&path, &raw).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(draft_path(&app)?);
    Ok(meta_from(&path, &raw))
}

// ---------------------------------------------------------------- draft

fn draft_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("draft.md"))
}

#[tauri::command]
fn save_draft(app: AppHandle, content: String) -> Result<(), String> {
    fs::write(draft_path(&app)?, content).map_err(|e| e.to_string())
}

#[tauri::command]
fn load_draft(app: AppHandle) -> String {
    draft_path(&app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_vault,
            set_vault,
            list_entries,
            read_entry,
            commit_entry,
            save_draft,
            load_draft
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
