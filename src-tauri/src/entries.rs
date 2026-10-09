use serde::Serialize;
use std::{fs, path::PathBuf};

#[derive(Serialize, Clone)]
pub(crate) struct EntryMeta {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) notebook: String,
    pub(crate) created: String,
    pub(crate) words: usize,
    pub(crate) preview: String,
    pub(crate) tags: Vec<String>,
    pub(crate) links: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct EntryFull {
    pub(crate) meta: EntryMeta,
    pub(crate) body: String,
    pub(crate) related: Vec<EntryMeta>,
}

#[derive(Serialize)]
pub(crate) struct TagCount {
    pub(crate) tag: String,
    pub(crate) count: usize,
}

/// Splits `---\n...\n---\n` frontmatter off the top of a file.
pub(crate) fn split_frontmatter(raw: &str) -> (Option<String>, String) {
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
pub(crate) fn clean_tag(raw: &str) -> String {
    let stripped: String = raw
        .trim()
        .trim_start_matches('#')
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ',' | '[' | ']' | '"' | '\n' | '\r'))
        .collect();
    stripped.trim().to_string()
}

pub(crate) fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = Vec::new();
    for item in items {
        if !item.is_empty() && !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

pub(crate) fn meta_from(path: &PathBuf, raw: &str) -> EntryMeta {
    let (fm, body) = split_frontmatter(raw);
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let fm = fm.unwrap_or_default();
    let preview: String = body
        .split_whitespace()
        .take(24)
        .collect::<Vec<_>>()
        .join(" ");

    EntryMeta {
        path: path.to_string_lossy().to_string(),
        created: fm_value(&fm, "created").unwrap_or_else(|| name.clone()),
        name,
        notebook: String::new(),
        words: body.split_whitespace().count(),
        preview,
        tags: fm_value(&fm, "tags")
            .map(|v| parse_list(&v))
            .unwrap_or_default(),
        links: fm_value(&fm, "links")
            .map(|v| parse_list(&v))
            .unwrap_or_default(),
    }
}

pub(crate) fn collect_entries(dir: &PathBuf) -> Vec<EntryMeta> {
    fn visit(root: &PathBuf, dir: &PathBuf, out: &mut Vec<EntryMeta>) {
        let Ok(listing) = fs::read_dir(dir) else {
            return;
        };
        for item in listing.flatten() {
            let path = item.path();
            let Ok(kind) = item.file_type() else {
                continue;
            };
            if kind.is_symlink() || item.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if kind.is_dir() {
                visit(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                if let Ok(raw) = fs::read_to_string(&path) {
                    let mut meta = meta_from(&path, &raw);
                    meta.notebook = path
                        .parent()
                        .and_then(|p| p.strip_prefix(root).ok())
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
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

/// Rewrites only the frontmatter. The body is carried across untouched, and
/// frontmatter keys this app does not know about are preserved verbatim.
pub(crate) fn rewrite_meta(
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

pub(crate) fn next_entry_path(root: &PathBuf, entry_dir: &PathBuf, stamp: &str) -> PathBuf {
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

pub(crate) fn collect_active_tags(dir: &PathBuf) -> Vec<String> {
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

pub(crate) fn stem_of(path: &PathBuf) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

pub(crate) fn add_link(path: &PathBuf, other: &str) -> Result<(), String> {
    let meta = meta_from(path, &fs::read_to_string(path).map_err(|e| e.to_string())?);
    let mut links = meta.links;
    links.push(other.to_string());
    rewrite_meta(path, None, Some(dedupe(links)))?;
    Ok(())
}

pub(crate) fn remove_link(path: &PathBuf, other: &str) -> Result<(), String> {
    let meta = meta_from(path, &fs::read_to_string(path).map_err(|e| e.to_string())?);
    let links: Vec<String> = meta.links.into_iter().filter(|l| l != other).collect();
    rewrite_meta(path, None, Some(links))?;
    Ok(())
}

/// Includes backlinks so a partially written link remains visible from both entries.
pub(crate) fn related_to<'a>(all: &'a [EntryMeta], me: &EntryMeta) -> Vec<&'a EntryMeta> {
    all.iter()
        .filter(|o| o.name != me.name && (me.links.contains(&o.name) || o.links.contains(&me.name)))
        .collect()
}

#[cfg(test)]
mod tests;
