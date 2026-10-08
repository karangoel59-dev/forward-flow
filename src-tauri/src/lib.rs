use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

// Protect the brief filesystem phase of Android checkouts against entry mutations.
static VAULT_WRITES: Mutex<()> = Mutex::new(());

mod vault_git;

// config

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

// entries

#[derive(Serialize, Clone)]
struct EntryMeta {
    path: String,
    name: String,
    notebook: String,
    created: String,
    words: usize,
    preview: String,
    tags: Vec<String>,
    links: Vec<String>,
}

#[derive(Serialize)]
struct EntryFull {
    meta: EntryMeta,
    body: String,
    related: Vec<EntryMeta>,
}

#[derive(Serialize)]
struct TagCount {
    tag: String,
    count: usize,
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

fn parse_list(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn render_list(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

/// Tags are lowercase, unadorned, and free of the characters that would
/// break the inline-list frontmatter encoding.
fn clean_tag(raw: &str) -> String {
    let stripped: String = raw
        .trim()
        .trim_start_matches('#')
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ',' | '[' | ']' | '"' | '\n' | '\r'))
        .collect();
    stripped.trim().to_string()
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = Vec::new();
    for item in items {
        if !item.is_empty() && !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
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
        notebook: String::new(),
        words: body.split_whitespace().count(),
        preview,
        tags: fm_value(&fm, "tags").map(|v| parse_list(&v)).unwrap_or_default(),
        links: fm_value(&fm, "links").map(|v| parse_list(&v)).unwrap_or_default(),
    }
}

fn collect_entries(dir: &PathBuf) -> Vec<EntryMeta> {
    fn visit(root: &PathBuf, dir: &PathBuf, out: &mut Vec<EntryMeta>) {
        let Ok(listing) = fs::read_dir(dir) else { return; };
        for item in listing.flatten() {
            let path = item.path();
            let Ok(kind) = item.file_type() else { continue; };
            if kind.is_symlink() || item.file_name().to_string_lossy().starts_with('.') { continue; }
            if kind.is_dir() { visit(root, &path, out); }
            else if path.extension().is_some_and(|e| e == "md") {
                if let Ok(raw) = fs::read_to_string(&path) {
                    let mut meta = meta_from(&path, &raw);
                    meta.notebook = path.parent().and_then(|p| p.strip_prefix(root).ok())
                        .map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                    out.push(meta);
                }
            }
        }
    }
    let mut out = Vec::new();
    let root = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    visit(&root, &root, &mut out);
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

fn notebook_dir(root: &PathBuf, notebook: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    if notebook.is_empty() { return root.canonicalize().map_err(|e| e.to_string()); }
    let relative = PathBuf::from(notebook);
    if notebook.contains('\\') || relative.components().any(|c| match c {
        Component::Normal(n) => n.to_string_lossy().starts_with('.'),
        _ => true,
    }) { return Err("invalid notebook folder".into()); }
    let vault = root.canonicalize().map_err(|e| e.to_string())?;
    let target = vault.join(relative).canonicalize().map_err(|e| e.to_string())?;
    if !target.starts_with(&vault) || !target.is_dir() { return Err("notebook is outside the vault".into()); }
    Ok(target)
}

fn collect_notebooks(root: &PathBuf) -> Result<Vec<String>, String> {
    fn visit(dir: &PathBuf, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
        for item in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            if item.file_name().to_string_lossy().starts_with('.') { continue; }
            if item.file_type().map_err(|e| e.to_string())?.is_dir() {
                let name = format!("{}{}", prefix, item.file_name().to_string_lossy());
                out.push(name.clone());
                visit(&item.path(), &format!("{name}/"), out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(root, "", &mut out)?;
    out.sort();
    Ok(out)
}

#[tauri::command]
fn list_notebooks(app: AppHandle) -> Result<Vec<String>, String> {
    collect_notebooks(&vault_dir(&app)?)
}

fn create_notebook_folder(root: &PathBuf, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('.') || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err("use a notebook name without path separators or special characters".into());
    }
    let path = root.join(name);
    fs::create_dir(&path).map_err(|e| e.to_string())?;
    // Git needs a file to preserve empty notebooks across devices.
    if let Err(error) = fs::write(path.join(".gitkeep"), "") {
        let _ = fs::remove_dir(&path);
        return Err(error.to_string());
    }
    Ok(name.into())
}

#[tauri::command]
fn create_notebook(app: AppHandle, name: String) -> Result<String, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let root = vault_dir(&app)?;
    let name = create_notebook_folder(&root, &name)?;
    vault_git::record(&app, root, format!("Create notebook {name}"), true);
    Ok(name)
}

fn move_entry_file(root: &PathBuf, source: &PathBuf, notebook: &str) -> Result<PathBuf, String> {
    let vault = root.canonicalize().map_err(|e| e.to_string())?;
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let relative = source.strip_prefix(&vault).map_err(|_| "entry is outside the vault")?;
    if !source.is_file() || source.extension().is_none_or(|e| e != "md")
        || relative.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
        return Err("only markdown entries can be moved".into());
    }
    let destination = notebook_dir(root, notebook)?.join(source.file_name().ok_or("invalid entry")?);
    if destination == source { return Ok(destination); }
    if destination.exists() { return Err("an entry with this filename already exists in that notebook".into()); }
    fs::rename(&source, &destination).map_err(|e| e.to_string())?;
    Ok(destination)
}

#[tauri::command]
fn move_entry(app: AppHandle, path: String, notebook: String) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let source = entry_in_vault(&app, &path)?;
    let root = vault_dir(&app)?;
    let raw = fs::read_to_string(&source).map_err(|e| e.to_string())?;
    let destination = move_entry_file(&root, &source, &notebook)?;
    let mut meta = meta_from(&destination, &raw);
    meta.notebook = notebook;
    vault_git::record(&app, root, format!("Move entry {} to notebook {}", meta.name, meta.notebook), true);
    Ok(meta)
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

// metadata rewriting

/// Rewrites only the frontmatter. The body is carried across untouched, and
/// frontmatter keys this app does not know about are preserved verbatim.
fn rewrite_meta(
    path: &PathBuf,
    tags: Option<Vec<String>>,
    links: Option<Vec<String>>,
) -> Result<EntryMeta, String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (fm, body) = split_frontmatter(&raw);
    let existing = fm.unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    let mut saw_tags = false;
    let mut saw_links = false;

    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("tags:") {
            saw_tags = true;
            match &tags {
                Some(t) => lines.push(format!("tags: {}", render_list(t))),
                None => lines.push(line.to_string()),
            }
        } else if trimmed.starts_with("links:") {
            saw_links = true;
            match &links {
                Some(l) => lines.push(format!("links: {}", render_list(l))),
                None => lines.push(line.to_string()),
            }
        } else {
            lines.push(line.to_string());
        }
    }

    if !saw_tags {
        if let Some(t) = &tags {
            lines.push(format!("tags: {}", render_list(t)));
        }
    }
    if !saw_links {
        if let Some(l) = &links {
            lines.push(format!("links: {}", render_list(l)));
        }
    }

    lines.retain(|l| !l.trim().is_empty());
    let out = format!("---\n{}\n---\n\n{}", lines.join("\n"), body);
    fs::write(path, &out).map_err(|e| e.to_string())?;
    Ok(meta_from(path, &out))
}

// commands

fn write_config(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let body = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(app)?, body).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_vault(app: AppHandle) -> Option<String> {
    if let Some(v) = read_config(&app).vault {
        return Some(v);
    }

    // Android has no folder picker; use private app storage and git for transfer.
    #[cfg(target_os = "android")]
    {
        let dir = app.path().app_data_dir().ok()?.join("vault");
        fs::create_dir_all(&dir).ok()?;
        let path = dir.to_string_lossy().to_string();
        write_config(&app, &Config { vault: Some(path.clone()) }).ok()?;
        vault_git::record(&app, dir, "Start Forward Flow vault".into(), true);
        return Some(path);
    }

    #[cfg(not(target_os = "android"))]
    None
}

#[tauri::command]
fn set_vault(app: AppHandle, path: String) -> Result<(), String> {
    fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    write_config(&app, &Config { vault: Some(path.clone()) })?;
    vault_git::record(&app, PathBuf::from(path), "Start Forward Flow vault".into(), true);
    Ok(())
}

#[tauri::command]
fn get_remote(app: AppHandle) -> Option<String> {
    vault_git::get_remote(&vault_dir(&app).ok()?)
}

#[tauri::command]
fn set_remote(app: AppHandle, url: String) -> Result<(), String> {
    vault_git::set_remote(&app, vault_dir(&app)?, url)
}

#[tauri::command]
fn resync_vault(app: AppHandle) -> Result<(), String> {
    let dir = vault_dir(&app)?;
    vault_git::sync_now(&app, dir, true);
    Ok(())
}

#[tauri::command]
fn list_entries(app: AppHandle) -> Result<Vec<EntryMeta>, String> {
    Ok(collect_entries(&vault_dir(&app)?))
}

#[tauri::command]
fn read_entry(app: AppHandle, path: String) -> Result<EntryFull, String> {
    let target = entry_in_vault(&app, &path)?;
    let raw = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let (_, body) = split_frontmatter(&raw);
    let mut meta = meta_from(&target, &raw);
    let root = vault_dir(&app)?.canonicalize().map_err(|e| e.to_string())?;
    meta.notebook = target.parent().and_then(|p| p.strip_prefix(&root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();

    let all = collect_entries(&vault_dir(&app)?);
    let related = related_to(&all, &meta).into_iter().cloned().collect();

    Ok(EntryFull {
        meta,
        body,
        related,
    })
}

fn next_entry_path(root: &PathBuf, entry_dir: &PathBuf, stamp: &str) -> PathBuf {
    let existing = collect_entries(root);
    let mut path = entry_dir.join(format!("{stamp}.md"));
    let mut n = 1;
    // Links use filename stems, so names must be unique across notebooks.
    while path.exists() || existing.iter().any(|e| e.name == stem_of(&path)) {
        path = entry_dir.join(format!("{stamp}-{n}.md"));
        n += 1;
    }
    path
}

/// Writes the draft to a new timestamped file and locks it. Never overwrites.
#[tauri::command]
fn commit_entry(app: AppHandle, content: String, notebook: Option<String>) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let dir = vault_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let notebook = notebook.unwrap_or_default();
    let entry_dir = notebook_dir(&dir, &notebook)?;
    let body = content.trim();
    if body.is_empty() {
        return Err("nothing written yet".into());
    }

    let now = chrono::Local::now();
    let stamp = now.format("%Y-%m-%d-%H%M%S").to_string();
    let path = next_entry_path(&dir, &entry_dir, &stamp);

    let raw = format!(
        "---\ncreated: {}\ntags: []\nlinks: []\n---\n\n{}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        body
    );
    fs::write(&path, &raw).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(draft_path(&app)?);
    // The entry is on disk; backing it up happens in the background and cannot fail the save.
    vault_git::record(&app, dir, format!("Add entry {}", stem_of(&path)), true);
    let mut meta = meta_from(&path, &raw);
    meta.notebook = notebook;
    Ok(meta)
}

fn collect_active_tags(dir: &PathBuf) -> Vec<String> {
    let mut tags = Vec::new();
    for entry in collect_entries(dir) {
        for tag in entry.tags {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    tags
}

#[tauri::command]
fn delete_entry(app: AppHandle, path: String) -> Result<(), String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let target = entry_in_vault(&app, &path)?;
    let raw = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let meta = meta_from(&target, &raw);
    let stem = meta.name.clone();
    let dir = vault_dir(&app)?;

    for other in collect_entries(&dir) {
        if other.name != stem && other.links.contains(&stem) {
            let p = PathBuf::from(&other.path);
            let _ = remove_link(&p, &stem);
        }
    }

    fs::remove_file(&target).map_err(|e| format!("failed to delete file: {}", e))?;

    vault_git::record(&app, dir.clone(), format!("Revert entry {}", stem), true);

    let _ = app.emit("vault-updated", ());
    Ok(())
}

#[tauri::command]
fn set_tags(app: AppHandle, path: String, tags: Vec<String>) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let target = entry_in_vault(&app, &path)?;
    let cleaned = dedupe(tags.iter().map(|t| clean_tag(t)).collect());
    let meta = rewrite_meta(&target, Some(cleaned), None)?;
    let dir = vault_dir(&app)?;
    vault_git::record(&app, dir.clone(), format!("Tag {}", meta.name), true);
    Ok(meta)
}

fn stem_of(path: &PathBuf) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().to_string()
}

fn add_link(path: &PathBuf, other: &str) -> Result<(), String> {
    let meta = meta_from(path, &fs::read_to_string(path).map_err(|e| e.to_string())?);
    let mut links = meta.links;
    links.push(other.to_string());
    rewrite_meta(path, None, Some(dedupe(links)))?;
    Ok(())
}

fn remove_link(path: &PathBuf, other: &str) -> Result<(), String> {
    let meta = meta_from(path, &fs::read_to_string(path).map_err(|e| e.to_string())?);
    let links: Vec<String> = meta.links.into_iter().filter(|l| l != other).collect();
    rewrite_meta(path, None, Some(links))?;
    Ok(())
}

/// Includes backlinks so a partially written link remains visible from both entries.
fn related_to<'a>(all: &'a [EntryMeta], me: &EntryMeta) -> Vec<&'a EntryMeta> {
    all.iter()
        .filter(|o| o.name != me.name && (me.links.contains(&o.name) || o.links.contains(&me.name)))
        .collect()
}

/// Links are symmetric: both files record the other.
#[tauri::command]
fn link_entries(app: AppHandle, a: String, b: String) -> Result<(), String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let pa = entry_in_vault(&app, &a)?;
    let pb = entry_in_vault(&app, &b)?;
    if pa == pb {
        return Err("an entry cannot link to itself".into());
    }

    add_link(&pa, &stem_of(&pb))?;
    add_link(&pb, &stem_of(&pa))?;
    vault_git::record(
        &app,
        vault_dir(&app)?,
        format!("Link {} and {}", stem_of(&pa), stem_of(&pb)),
        true,
    );
    Ok(())
}

#[tauri::command]
fn unlink_entries(app: AppHandle, a: String, b: String) -> Result<(), String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let pa = entry_in_vault(&app, &a)?;
    let pb = entry_in_vault(&app, &b)?;

    remove_link(&pa, &stem_of(&pb))?;
    remove_link(&pb, &stem_of(&pa))?;
    vault_git::record(
        &app,
        vault_dir(&app)?,
        format!("Unlink {} and {}", stem_of(&pa), stem_of(&pb)),
        true,
    );
    Ok(())
}

#[tauri::command]
fn all_tags(app: AppHandle) -> Result<Vec<TagCount>, String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in collect_entries(&vault_dir(&app)?) {
        for tag in entry.tags {
            *counts.entry(tag).or_insert(0) += 1;
        }
    }
    let mut out: Vec<TagCount> = counts
        .into_iter()
        .map(|(tag, count)| TagCount { tag, count })
        .collect();
    // Commonest first, then alphabetical.
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));
    Ok(out)
}

// draft

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
        .setup(|app| {
            #[cfg(all(target_os = "android", feature = "tls-diagnostics"))]
            if let Ok(dir) = app.path().app_data_dir() {
                vault_git::diagnose_tls(dir);
            }
            // Commit external changes and retry pending pushes on launch.
            if let Ok(vault) = vault_dir(app.handle()) {
                vault_git::record(app.handle(), vault.clone(), "Sync vault".into(), false);
            }
            vault_git::start_background_sync(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_vault,
            set_vault,
            get_remote,
            set_remote,
            resync_vault,
            list_entries,
            list_notebooks,
            create_notebook,
            move_entry,
            read_entry,
            commit_entry,
            delete_entry,
            set_tags,
            link_entries,
            unlink_entries,
            all_tags,
            save_draft,
            load_draft
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// tests

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, contents: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ff-test-{}.md", name));
        fs::write(&p, contents).unwrap();
        p
    }

    const SAMPLE: &str = "---\ncreated: 2026-09-21T01:08:07+05:30\ntags: []\nlinks: []\n---\n\nWorking fine\n\nLooks good\n";

    fn notebook_vault(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ff-notebook-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn notebooks_include_nested_entries_and_skip_hidden_files() {
        let root = notebook_vault("discovery");
        create_notebook_folder(&root, " Work ").unwrap();
        fs::create_dir_all(root.join("Work/Ideas")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("a.md"), SAMPLE).unwrap();
        fs::write(root.join("Work/Ideas/b.md"), SAMPLE).unwrap();
        fs::write(root.join(".git/hidden.md"), SAMPLE).unwrap();
        fs::write(root.join("Work/.hidden.md"), SAMPLE).unwrap();
        assert!(root.join("Work/.gitkeep").exists());
        assert_eq!(collect_notebooks(&root).unwrap(), vec!["Work", "Work/Ideas"]);
        let entries = collect_entries(&root);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].notebook, "Work/Ideas");
        assert_eq!(entries[1].notebook, "");
        assert!(create_notebook_folder(&root, "Work").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_between_notebooks_preserves_bytes_and_links() {
        let root = notebook_vault("move");
        create_notebook_folder(&root, "Ideas").unwrap();
        let a = root.join("a.md");
        let b = root.join("b.md");
        fs::write(&a, SAMPLE).unwrap();
        fs::write(&b, SAMPLE).unwrap();
        add_link(&a, "b").unwrap();
        add_link(&b, "a").unwrap();
        let before = fs::read(&a).unwrap();
        let moved = move_entry_file(&root, &a, "Ideas").unwrap();
        assert_eq!(fs::read(&moved).unwrap(), before);
        assert!(!a.exists());
        let entries = collect_entries(&root);
        let a_meta = entries.iter().find(|e| e.name == "a").unwrap();
        assert_eq!(related_to(&entries, a_meta)[0].name, "b");
        assert_eq!(move_entry_file(&root, &moved, "").unwrap(), a);
        assert_eq!(fs::read(&a).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn new_entry_names_remain_unique_across_notebooks() {
        let root = notebook_vault("filenames");
        create_notebook_folder(&root, "Ideas").unwrap();
        fs::write(root.join("stamp.md"), SAMPLE).unwrap();
        fs::write(root.join("Ideas/stamp-1.md"), SAMPLE).unwrap();
        let path = next_entry_path(&root, &root.join("Ideas"), "stamp");
        assert_eq!(path, root.join("Ideas/stamp-2.md"));
        assert_eq!(fs::read_to_string(root.join("stamp.md")).unwrap(), SAMPLE);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notebook_move_collision_keeps_both_entries() {
        let root = notebook_vault("collision");
        create_notebook_folder(&root, "Ideas").unwrap();
        let source = root.join("a.md");
        let target = root.join("Ideas/a.md");
        fs::write(&source, "first").unwrap();
        fs::write(&target, "second").unwrap();
        assert!(move_entry_file(&root, &source, "Ideas").is_err());
        assert_eq!(fs::read_to_string(source).unwrap(), "first");
        assert_eq!(fs::read_to_string(target).unwrap(), "second");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notebook_paths_reject_traversal_and_internal_files() {
        let root = notebook_vault("validation");
        for name in ["", ".git", "../outside", "a/b", "a\\b", "bad:name"] {
            assert!(create_notebook_folder(&root, name).is_err(), "{name}");
        }
        for name in ["../", ".git", "/tmp", "a\\b"] {
            assert!(notebook_dir(&root, name).is_err(), "{name}");
        }
        fs::create_dir(root.join(".git")).unwrap();
        let internal = root.join(".git/config.md");
        fs::write(&internal, "secret").unwrap();
        assert!(move_entry_file(&root, &internal, "").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn notebook_discovery_does_not_follow_symlinks() {
        let root = notebook_vault("symlinks");
        let outside = notebook_vault("outside");
        fs::write(outside.join("a.md"), SAMPLE).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("Linked")).unwrap();
        assert!(collect_notebooks(&root).unwrap().is_empty());
        assert!(collect_entries(&root).is_empty());
        assert!(notebook_dir(&root, "Linked").is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn tagging_leaves_the_body_byte_for_byte() {
        let p = scratch("body", SAMPLE);
        let (_, before) = split_frontmatter(&fs::read_to_string(&p).unwrap());

        rewrite_meta(&p, Some(vec!["rivers".into(), "tooling".into()]), None).unwrap();

        let (_, after) = split_frontmatter(&fs::read_to_string(&p).unwrap());
        assert_eq!(before, after, "body must survive a metadata write untouched");
    }

    #[test]
    fn tags_and_links_round_trip() {
        let p = scratch("round", SAMPLE);
        rewrite_meta(&p, Some(vec!["alpha".into()]), Some(vec!["2026-01-01-000000".into()]))
            .unwrap();

        let meta = meta_from(&p, &fs::read_to_string(&p).unwrap());
        assert_eq!(meta.tags, vec!["alpha".to_string()]);
        assert_eq!(meta.links, vec!["2026-01-01-000000".to_string()]);
    }

    #[test]
    fn unknown_frontmatter_keys_are_preserved() {
        let raw = "---\ncreated: 2026-09-21T01:08:07+05:30\nmood: restless\ntags: []\n---\n\nbody\n";
        let p = scratch("unknown", raw);
        rewrite_meta(&p, Some(vec!["x".into()]), None).unwrap();

        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("mood: restless"), "foreign keys must not be dropped");
        assert!(out.contains("created: 2026-09-21T01:08:07+05:30"));
    }

    #[test]
    fn prose_that_looks_like_frontmatter_is_not_rewritten() {
        let raw = "---\ncreated: x\ntags: []\n---\n\ntags: this is prose, not metadata\n";
        let p = scratch("lookalike", raw);
        rewrite_meta(&p, Some(vec!["real".into()]), None).unwrap();

        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("tags: this is prose, not metadata"));
        assert!(out.contains("tags: [real]"));
    }

    #[test]
    fn repeated_writes_are_stable() {
        let p = scratch("stable", SAMPLE);
        rewrite_meta(&p, Some(vec!["a".into()]), None).unwrap();
        let once = fs::read_to_string(&p).unwrap();
        rewrite_meta(&p, Some(vec!["a".into()]), None).unwrap();
        let twice = fs::read_to_string(&p).unwrap();
        assert_eq!(once, twice, "rewriting the same metadata must be idempotent");
    }

    #[test]
    fn tags_are_normalised() {
        assert_eq!(clean_tag("  #Rivers "), "rivers");
        assert_eq!(clean_tag("Half[Baked]"), "halfbaked");
        assert_eq!(clean_tag("a,b"), "ab");
    }

    #[test]
    fn dedupe_drops_repeats_and_blanks() {
        let got = dedupe(vec!["a".into(), "".into(), "a".into(), "b".into()]);
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
    }

    fn meta(name: &str, links: &[&str]) -> EntryMeta {
        EntryMeta {
            path: format!("/tmp/{}.md", name),
            name: name.into(),
            notebook: String::new(),
            created: name.into(),
            words: 0,
            preview: String::new(),
            tags: vec![],
            links: links.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn linking_is_symmetric_and_reversible() {
        let a = scratch("link-a", SAMPLE);
        let b = scratch("link-b", SAMPLE);
        let (na, nb) = (stem_of(&a), stem_of(&b));

        add_link(&a, &nb).unwrap();
        add_link(&b, &na).unwrap();
        assert_eq!(meta_from(&a, &fs::read_to_string(&a).unwrap()).links, vec![nb.clone()]);
        assert_eq!(meta_from(&b, &fs::read_to_string(&b).unwrap()).links, vec![na.clone()]);

        remove_link(&a, &nb).unwrap();
        remove_link(&b, &na).unwrap();
        assert!(meta_from(&a, &fs::read_to_string(&a).unwrap()).links.is_empty());
        assert!(meta_from(&b, &fs::read_to_string(&b).unwrap()).links.is_empty());
    }

    #[test]
    fn linking_twice_does_not_duplicate() {
        let a = scratch("dup-a", SAMPLE);
        add_link(&a, "somewhere").unwrap();
        add_link(&a, "somewhere").unwrap();
        assert_eq!(meta_from(&a, &fs::read_to_string(&a).unwrap()).links.len(), 1);
    }

    #[test]
    fn related_includes_backlinks_from_a_half_written_pair() {
        let all = vec![meta("one", &["two"]), meta("two", &[]), meta("three", &[])];

        // Forward direction: one -> two.
        let from_one: Vec<_> = related_to(&all, &all[0]).iter().map(|e| &e.name).collect();
        assert_eq!(from_one, vec!["two"]);

        // Two records nothing, but must still see one.
        let from_two: Vec<_> = related_to(&all, &all[1]).iter().map(|e| &e.name).collect();
        assert_eq!(from_two, vec!["one"], "a one-sided link must surface on both sides");

        assert!(related_to(&all, &all[2]).is_empty());
    }

    #[test]
    fn an_entry_is_never_related_to_itself() {
        let all = vec![meta("solo", &["solo"])];
        assert!(related_to(&all, &all[0]).is_empty());
    }

    #[test]
    fn empty_list_parses_to_nothing() {
        assert!(parse_list("[]").is_empty());
        assert_eq!(parse_list("[a, b]"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(render_list(&["a".into(), "b".into()]), "[a, b]");
    }

    #[test]
    fn reciprocal_links_are_cleaned_up_on_entry_removal() {
        let dir = std::env::temp_dir().join(format!("ff-del-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let a_path = dir.join("2026-09-21-000001.md");
        let b_path = dir.join("2026-09-21-000002.md");
        let na = stem_of(&a_path);
        let nb = stem_of(&b_path);

        fs::write(&a_path, format!("---\ncreated: x\ntags: []\nlinks: [{}]\n---\n\nEntry A\n", nb)).unwrap();
        fs::write(&b_path, format!("---\ncreated: x\ntags: []\nlinks: [{}]\n---\n\nEntry B\n", na)).unwrap();

        // Simulate deleting A:
        for other in collect_entries(&dir) {
            if other.name != na && other.links.contains(&na) {
                let p = PathBuf::from(&other.path);
                let _ = remove_link(&p, &na);
            }
        }
        fs::remove_file(&a_path).unwrap();

        assert!(!a_path.exists());
        let b_meta = meta_from(&b_path, &fs::read_to_string(&b_path).unwrap());
        assert!(b_meta.links.is_empty(), "Entry B's reciprocal link to A must be removed");

        fs::remove_dir_all(&dir).unwrap();
    }
}
