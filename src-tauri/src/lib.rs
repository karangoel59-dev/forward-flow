use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

mod vault_git;

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
        words: body.split_whitespace().count(),
        preview,
        tags: fm_value(&fm, "tags").map(|v| parse_list(&v)).unwrap_or_default(),
        links: fm_value(&fm, "links").map(|v| parse_list(&v)).unwrap_or_default(),
    }
}

fn collect_entries(dir: &PathBuf) -> Vec<EntryMeta> {
    let mut out = Vec::new();
    let listing = match fs::read_dir(dir) {
        Ok(l) => l,
        Err(_) => return out,
    };
    for item in listing.flatten() {
        let path = item.path();
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
    out
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

// ------------------------------------------------- metadata rewriting

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

// ---------------------------------------------------------------- commands

fn write_config(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let body = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(app)?, body).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_vault(app: AppHandle) -> Option<String> {
    if let Some(v) = read_config(&app).vault {
        return Some(v);
    }

    // tauri-plugin-dialog has no folder picker on mobile — its own source rejects a
    // directory-mode `open()` outright (`Err(FolderPickerNotImplemented)`, by design in that
    // crate, not a bug here). Rather than strand Android on the desktop setup screen waiting
    // for a picker that will never appear, default straight to the app's own private storage;
    // git sync to a remote (vault_git) is how entries leave an Android device, not picking a
    // shared folder.
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
fn list_entries(app: AppHandle) -> Result<Vec<EntryMeta>, String> {
    Ok(collect_entries(&vault_dir(&app)?))
}

#[tauri::command]
fn read_entry(app: AppHandle, path: String) -> Result<EntryFull, String> {
    let target = entry_in_vault(&app, &path)?;
    let raw = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    let (_, body) = split_frontmatter(&raw);
    let meta = meta_from(&target, &raw);

    // Union of this entry's own links and anything pointing back at it, so a
    // half-written pair still shows up on both sides.
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
        "---\ncreated: {}\ntags: []\nlinks: []\n---\n\n{}\n",
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        body
    );
    fs::write(&path, &raw).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(draft_path(&app)?);
    // The entry is on disk; backing it up happens in the background and cannot fail the save.
    vault_git::record(&app, dir, format!("Add entry {}", stem_of(&path)), true);
    Ok(meta_from(&path, &raw))
}

#[tauri::command]
fn set_tags(app: AppHandle, path: String, tags: Vec<String>) -> Result<EntryMeta, String> {
    let target = entry_in_vault(&app, &path)?;
    let cleaned = dedupe(tags.iter().map(|t| clean_tag(t)).collect());
    let meta = rewrite_meta(&target, Some(cleaned), None)?;
    vault_git::record(&app, vault_dir(&app)?, format!("Tag {}", meta.name), true);
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

/// Anything this entry points at, plus anything pointing back at it, so a
/// half-written pair still surfaces on both sides.
fn related_to<'a>(all: &'a [EntryMeta], me: &EntryMeta) -> Vec<&'a EntryMeta> {
    all.iter()
        .filter(|o| o.name != me.name && (me.links.contains(&o.name) || o.links.contains(&me.name)))
        .collect()
}

/// Links are symmetric: both files record the other.
#[tauri::command]
fn link_entries(app: AppHandle, a: String, b: String) -> Result<(), String> {
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
        .setup(|app| {
            // Anything written outside the app, or a push that failed last time, goes out now.
            // Only problems are reported: a quiet launch should stay quiet.
            if let Ok(vault) = vault_dir(app.handle()) {
                vault_git::record(app.handle(), vault, "Sync vault".into(), false);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_vault,
            set_vault,
            list_entries,
            read_entry,
            commit_entry,
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

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, contents: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ff-test-{}.md", name));
        fs::write(&p, contents).unwrap();
        p
    }

    const SAMPLE: &str = "---\ncreated: 2026-09-21T01:08:07+05:30\ntags: []\nlinks: []\n---\n\nWorking fine\n\nLooks good\n";

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
}
