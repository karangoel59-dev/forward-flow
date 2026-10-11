use std::{fs, path::PathBuf};
pub(crate) fn notebook_dir(root: &PathBuf, notebook: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    if notebook.is_empty() {
        return root.canonicalize().map_err(|e| e.to_string());
    }
    let relative = PathBuf::from(notebook);
    if notebook.contains('\\')
        || relative.components().any(|c| match c {
            Component::Normal(n) => n.to_string_lossy().starts_with('.'),
            _ => true,
        })
    {
        return Err("invalid notebook folder".into());
    }
    let vault = root.canonicalize().map_err(|e| e.to_string())?;
    let target = vault
        .join(relative)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !target.starts_with(&vault) || !target.is_dir() {
        return Err("notebook is outside the vault".into());
    }
    Ok(target)
}

pub(crate) fn collect_notebooks(root: &PathBuf) -> Result<Vec<String>, String> {
    fn visit(dir: &PathBuf, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
        for item in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            if item.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
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

pub(crate) fn create_notebook_folder(root: &PathBuf, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.starts_with('.')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
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

pub(crate) fn move_entry_file(
    root: &PathBuf,
    source: &PathBuf,
    notebook: &str,
) -> Result<PathBuf, String> {
    let vault = root.canonicalize().map_err(|e| e.to_string())?;
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let relative = source
        .strip_prefix(&vault)
        .map_err(|_| "entry is outside the vault")?;
    if !source.is_file()
        || source.extension().is_none_or(|e| e != "md")
        || relative
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        return Err("only markdown entries can be moved".into());
    }
    let destination =
        notebook_dir(root, notebook)?.join(source.file_name().ok_or("invalid entry")?);
    if destination == source {
        return Ok(destination);
    }
    if destination.exists() {
        return Err("an entry with this filename already exists in that notebook".into());
    }
    fs::rename(&source, &destination).map_err(|e| e.to_string())?;
    Ok(destination)
}

#[cfg(test)]
mod tests;

// Limits are UTF-8 / serialized JSON bytes, not character counts.
pub(crate) const NOTEBOOK_LIMIT: usize = 64_000;

pub(crate) fn metadata_path(dir: &PathBuf) -> Result<PathBuf, String> {
    let path = dir.join(".notebook.json");
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => {
            Err("Notebook metadata must be a regular file".into())
        }
        Ok(meta) if meta.len() > NOTEBOOK_LIMIT as u64 => {
            Err("Notebook metadata exceeds 64000 bytes".into())
        }
        Ok(_) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(_) => Err("Cannot inspect notebook metadata".into()),
    }
}

/// Keep the exact source for stale guards; None distinguishes a missing sidecar.
pub(crate) fn read_metadata(dir: &PathBuf) -> Result<(Option<String>, serde_json::Value), String> {
    let path = metadata_path(dir)?;
    use std::io::Read;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, serde_json::json!({})))
        }
        Err(_) => return Err("Cannot read notebook metadata".into()),
    };
    let mut raw = String::new();
    file.take((NOTEBOOK_LIMIT + 1) as u64)
        .read_to_string(&mut raw)
        .map_err(|_| "Cannot read notebook metadata as UTF-8")?;
    if raw.len() > NOTEBOOK_LIMIT {
        return Err("Notebook metadata exceeds 64000 bytes".into());
    }
    let data: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "Notebook metadata file is invalid JSON")?;
    if !data.is_object() || data.get("purpose").is_some_and(|p| !p.is_string()) {
        return Err("Notebook metadata must be an object with a string purpose".into());
    }
    Ok((Some(raw), data))
}

pub(crate) fn read_purpose(dir: &PathBuf) -> Result<String, String> {
    Ok(read_metadata(dir)?.1["purpose"]
        .as_str()
        .unwrap_or_default()
        .into())
}

fn valid_metadata_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.as_bytes()[0].is_ascii_alphabetic()
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        && ![
            "__proto__",
            "prototype",
            "constructor",
            "purpose",
            "metadata",
            "created",
            "tags",
            "links",
            "path",
            "filename",
            "name",
            "notebook",
            "id",
            "body",
            "content",
        ]
        .contains(&key.to_ascii_lowercase().as_str())
}

/// Patch only purpose and a flat namespace of user values. Null removes a user key.
pub(crate) fn updated_metadata(
    existing: &serde_json::Value,
    patch: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let patch = patch
        .as_object()
        .ok_or("Metadata update must be an object")?;
    if patch.is_empty() || patch.keys().any(|k| k != "purpose" && k != "metadata") {
        return Err("Update only purpose or metadata".into());
    }
    let mut out = existing
        .as_object()
        .ok_or("Notebook metadata must be an object")?
        .clone();
    if let Some(purpose) = patch.get("purpose") {
        if purpose.as_str().is_none_or(|p| p.len() > 12_000) {
            return Err("Purpose must be a string of at most 12000 bytes".into());
        }
        out.insert("purpose".into(), purpose.clone());
    }
    if let Some(values) = patch.get("metadata") {
        let values = values.as_object().ok_or("Metadata must be an object")?;
        if values.is_empty() || values.len() > 50 {
            return Err("Supply 1 to 50 metadata keys".into());
        }
        let mut custom = match out.get("metadata") {
            None => serde_json::Map::new(),
            Some(value) => value
                .as_object()
                .ok_or("Existing metadata must be an object")?
                .clone(),
        };
        for (key, value) in values {
            if !valid_metadata_key(key) {
                return Err("Invalid or reserved metadata key".into());
            }
            match value {
                serde_json::Value::Null => { custom.remove(key); }
                serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                    custom.insert(key.clone(), value.clone());
                }
                serde_json::Value::String(text) if text.len() <= 4_000 => {
                    custom.insert(key.clone(), value.clone());
                }
                _ => return Err("Metadata values must be strings (up to 4000 bytes), numbers, booleans, or null for removal".into()),
            }
        }
        let custom = serde_json::Value::Object(custom);
        if custom.as_object().unwrap().len() > 50
            || serde_json::to_vec(&custom).unwrap().len() > 16_000
        {
            return Err("Metadata is limited to 50 keys and 16000 serialized bytes".into());
        }
        out.insert("metadata".into(), custom);
    }
    let out = serde_json::Value::Object(out);
    if serde_json::to_vec_pretty(&out).unwrap().len() > NOTEBOOK_LIMIT {
        return Err("Notebook metadata exceeds 64000 bytes".into());
    }
    Ok(out)
}

pub(crate) fn write_metadata(dir: &PathBuf, data: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let path = metadata_path(dir)?;
    let bytes = serde_json::to_vec_pretty(data).map_err(|_| "Cannot encode notebook metadata")?;
    if bytes.len() > NOTEBOOK_LIMIT {
        return Err("Notebook metadata exceeds 64000 bytes".into());
    }
    // Exclusive creation avoids following a pre-existing temporary-file symlink.
    let temporary = dir.join(format!(
        ".notebook-{}.tmp",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| "Cannot stage notebook metadata")?;
    let result = (|| -> Result<(), String> {
        file.write_all(&bytes)
            .map_err(|_| "Cannot save notebook metadata")?;
        file.sync_all()
            .map_err(|_| "Cannot save notebook metadata")?;
        drop(file);
        metadata_path(dir)?;
        fs::rename(&temporary, &path).map_err(|_| "Cannot replace notebook metadata".to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub(crate) fn write_purpose(dir: &PathBuf, purpose: &str) -> Result<(), String> {
    let (_, existing) = read_metadata(dir)?;
    let updated = updated_metadata(&existing, &serde_json::json!({"purpose": purpose}))?;
    write_metadata(dir, &updated)
}
