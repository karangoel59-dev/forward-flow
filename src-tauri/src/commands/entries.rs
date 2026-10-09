use crate::{
    config::vault_dir,
    drafts::draft_path,
    entries::{
        add_link, clean_tag, collect_entries, dedupe, meta_from, next_entry_path, related_to,
        remove_link, rewrite_meta, split_frontmatter, stem_of, EntryFull, EntryMeta, TagCount,
    },
    notebooks::notebook_dir,
    state::VAULT_WRITES,
    vault_git,
};
use std::{collections::BTreeMap, fs, path::PathBuf};
use tauri::{AppHandle, Emitter};
/// Rejects paths that resolve outside the vault.
pub(super) fn entry_in_vault(app: &AppHandle, path: &str) -> Result<PathBuf, String> {
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
pub(crate) fn list_entries(app: AppHandle) -> Result<Vec<EntryMeta>, String> {
    Ok(collect_entries(&vault_dir(&app)?))
}

#[tauri::command]
pub(crate) fn read_entry(app: AppHandle, path: String) -> Result<EntryFull, String> {
    let target = entry_in_vault(&app, &path)?;
    let raw = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let (_, body) = split_frontmatter(&raw);
    let mut meta = meta_from(&target, &raw);
    let root = vault_dir(&app)?.canonicalize().map_err(|e| e.to_string())?;
    meta.notebook = target
        .parent()
        .and_then(|p| p.strip_prefix(&root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    let all = collect_entries(&vault_dir(&app)?);
    let related = related_to(&all, &meta).into_iter().cloned().collect();

    Ok(EntryFull {
        meta,
        body,
        related,
    })
}

/// Writes the draft to a new timestamped file and locks it. Never overwrites.
#[tauri::command]
pub(crate) fn commit_entry(
    app: AppHandle,
    content: String,
    notebook: Option<String>,
) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let dir = vault_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let notebook = notebook.unwrap_or_default();
    let entry_dir = notebook_dir(&dir, &notebook)?;
    let body = content.trim();
    if body.is_empty() {
        return Err("nothing written yet".into());
    }

    let meta = write_page(&dir, &entry_dir, body, notebook)?;
    let _ = fs::remove_file(draft_path(&app)?);
    vault_git::record(&app, dir, format!("Add entry {}", meta.name), true);
    Ok(meta)
}

fn write_page(
    dir: &PathBuf,
    entry_dir: &PathBuf,
    body: &str,
    notebook: String,
) -> Result<EntryMeta, String> {
    let now = chrono::Local::now();
    let stamp = now.format("%Y-%m-%d-%H%M%S").to_string();
    let path = next_entry_path(dir, entry_dir, &stamp);
    let raw = format!(
        "---\ncreated: {}\ntags: []\nlinks: []\n---\n\n{}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        body
    );
    fs::write(&path, &raw).map_err(|e| e.to_string())?;
    let mut meta = meta_from(&path, &raw);
    meta.notebook = notebook;
    Ok(meta)
}

#[tauri::command]
pub(crate) fn save_chat_page(
    app: AppHandle,
    notebook: String,
    content: String,
) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let root = vault_dir(&app)?;
    let dir = notebook_dir(&root, &notebook)?;
    if content.trim().is_empty() {
        return Err("Write a page before saving".into());
    }
    let meta = write_page(&root, &dir, content.trim(), notebook)?;
    vault_git::record(&app, root, format!("Add chat page {}", meta.name), true);
    Ok(meta)
}

#[tauri::command]
pub(crate) fn delete_entry(app: AppHandle, path: String) -> Result<(), String> {
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
pub(crate) fn set_tags(
    app: AppHandle,
    path: String,
    tags: Vec<String>,
) -> Result<EntryMeta, String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let target = entry_in_vault(&app, &path)?;
    let cleaned = dedupe(tags.iter().map(|t| clean_tag(t)).collect());
    let meta = rewrite_meta(&target, Some(cleaned), None)?;
    let dir = vault_dir(&app)?;
    vault_git::record(&app, dir.clone(), format!("Tag {}", meta.name), true);
    Ok(meta)
}

/// Links are symmetric: both files record the other.
#[tauri::command]
pub(crate) fn link_entries(app: AppHandle, a: String, b: String) -> Result<(), String> {
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
pub(crate) fn unlink_entries(app: AppHandle, a: String, b: String) -> Result<(), String> {
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
pub(crate) fn all_tags(app: AppHandle) -> Result<Vec<TagCount>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_pages_are_new_files_and_preserve_existing_entries() {
        let root = std::env::temp_dir().join(format!("ff-chat-pages-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Ideas")).unwrap();
        let body = "# Draft\n\nA new idea.";
        let first = write_page(&root, &root.join("Ideas"), body, "Ideas".into()).unwrap();
        let second =
            write_page(&root, &root.join("Ideas"), "Another page", "Ideas".into()).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(first.notebook, "Ideas");
        let (_, saved) = split_frontmatter(&fs::read_to_string(&first.path).unwrap());
        assert_eq!(saved, format!("{body}\n"));
        fs::remove_dir_all(root).unwrap();
    }
}
