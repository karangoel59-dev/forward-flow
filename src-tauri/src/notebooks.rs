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

pub(crate) fn read_purpose(dir: &PathBuf) -> Result<String, String> {
    let path = purpose_path(dir)?;
    if !path.exists() {
        return Ok(String::new());
    }
    let raw = fs::read_to_string(path).map_err(|_| "Cannot read notebook purpose")?;
    let data: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "Notebook purpose file is invalid")?;
    Ok(data["purpose"].as_str().unwrap_or_default().into())
}

fn purpose_path(dir: &PathBuf) -> Result<PathBuf, String> {
    let path = dir.join(".notebook.json");
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Notebook purpose must be a regular file".into());
    }
    Ok(path)
}

pub(crate) fn write_purpose(dir: &PathBuf, purpose: &str) -> Result<(), String> {
    if purpose.len() > 12_000 {
        return Err("Notebook purpose is too long".into());
    }
    fs::write(
        purpose_path(dir)?,
        serde_json::to_vec_pretty(&serde_json::json!({"purpose":purpose})).unwrap(),
    )
    .map_err(|_| "Cannot save notebook purpose".into())
}
