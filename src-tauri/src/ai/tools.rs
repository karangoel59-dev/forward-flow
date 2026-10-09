use super::providers::ToolCall;
use crate::{
    entries::{
        add_link, clean_tag, collect_entries, dedupe, meta_from, next_entry_path, remove_link,
        rewrite_meta, split_frontmatter,
    },
    notebooks::{create_notebook_folder, move_entry_file, notebook_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Proposal {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub before: Vec<(String, String)>,
    #[serde(default)]
    pub applied: bool,
}

pub(crate) fn definitions() -> Vec<Value> {
    let s = json!({"type":"string"});
    let a = json!({"type":"array","items":{"type":"string"}});
    let specs = vec![
        (
            "list_entries",
            "List pages in this notebook",
            json!({}),
            vec![],
        ),
        (
            "search_entries",
            "Search notebook page text and tags",
            json!({"query":s}),
            vec!["query"],
        ),
        (
            "read_entry",
            "Read a page by filename",
            json!({"filename":s}),
            vec!["filename"],
        ),
        (
            "create_entry",
            "Propose a new Markdown page",
            json!({"content":s}),
            vec!["content"],
        ),
        (
            "revise_entry",
            "Propose a linked revision; original is preserved",
            json!({"filename":s,"content":s}),
            vec!["filename", "content"],
        ),
        (
            "delete_entry",
            "Propose deleting a page; requires user review",
            json!({"filename":s}),
            vec!["filename"],
        ),
        (
            "set_tags",
            "Propose replacing page tags",
            json!({"filename":s,"tags":a}),
            vec!["filename", "tags"],
        ),
        (
            "link_entries",
            "Propose linking two notebook pages",
            json!({"filename":s,"other":s}),
            vec!["filename", "other"],
        ),
        (
            "unlink_entries",
            "Propose unlinking two notebook pages",
            json!({"filename":s,"other":s}),
            vec!["filename", "other"],
        ),
        (
            "create_notebook",
            "Propose a child notebook",
            json!({"name":s}),
            vec!["name"],
        ),
        (
            "move_entry",
            "Propose moving a page into a child notebook",
            json!({"filename":s,"destination":s}),
            vec!["filename", "destination"],
        ),
        (
            "git_status",
            "Inspect changed files in this notebook",
            json!({}),
            vec![],
        ),
        (
            "git_history",
            "Inspect the last ten vault commit messages",
            json!({}),
            vec![],
        ),
        (
            "sync_vault",
            "Propose syncing the vault through the app",
            json!({}),
            vec![],
        ),
    ];
    specs.into_iter().map(|(name,description,properties,required)|json!({"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("Missing {key}"))
}
fn page(root: &PathBuf, notebook: &str, filename: &str) -> Result<PathBuf, String> {
    collect_entries(root)
        .into_iter()
        .find(|e| e.notebook == notebook && format!("{}.md", e.name) == filename)
        .map(|e| PathBuf::from(e.path))
        .ok_or("Page is not in the selected notebook".into())
}
fn child(dir: &PathBuf, name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.starts_with('.')
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err("Use a simple child notebook name".into());
    }
    let target = dir.join(name);
    if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Cannot use a linked folder".into());
    }
    Ok(())
}

pub(crate) fn execute(
    root: &PathBuf,
    notebook: &str,
    call: &ToolCall,
    id: String,
) -> Result<(Value, Option<Proposal>), String> {
    let definition = definitions()
        .into_iter()
        .find(|d| d["name"] == call.name)
        .ok_or("Unknown tool")?;
    for key in call
        .arguments
        .as_object()
        .ok_or("Invalid arguments")?
        .keys()
    {
        if definition["parameters"]["properties"].get(key).is_none() {
            return Err("Unknown tool argument".into());
        }
    }
    let dir = notebook_dir(root, notebook)?;
    let args = &call.arguments;
    match call.name.as_str() {
        "list_entries" | "search_entries" => {
            let query = if call.name == "search_entries" {
                string(args, "query")?.to_lowercase()
            } else {
                String::new()
            };
            let mut found = vec![];
            for e in collect_entries(root)
                .into_iter()
                .filter(|e| e.notebook == notebook)
            {
                if found.len() >= 100 {
                    break;
                }
                let body = if query.is_empty()
                    || fs::metadata(&e.path)
                        .map_err(|_| "Cannot inspect page")?
                        .len()
                        > 60_000
                {
                    String::new()
                } else {
                    fs::read_to_string(&e.path).map_err(|_| "Cannot read page")?
                };
                if query.is_empty()
                    || format!("{} {} {}", e.name, body, e.tags.join(" "))
                        .to_lowercase()
                        .contains(&query)
                {
                    found.push(json!({"filename":format!("{}.md",e.name),"preview":e.preview,"tags":e.tags,"links":e.links}));
                }
            }
            return Ok((json!({"entries":found,"limit":100}), None));
        }
        "read_entry" => {
            let p = page(root, notebook, string(args, "filename")?)?;
            if fs::metadata(&p).map_err(|_| "Cannot read page")?.len() > 60_000 {
                return Err("Page exceeds the tool read limit".into());
            }
            let raw = fs::read_to_string(&p).map_err(|_| "Cannot read page")?;
            return Ok((
                json!({"filename":args["filename"],"body":split_frontmatter(&raw).1}),
                None,
            ));
        }
        "git_status" | "git_history" => {
            let repo =
                git2::Repository::open(root).map_err(|_| "Vault Git repository is unavailable")?;
            if call.name == "git_status" {
                let statuses = repo.statuses(None).map_err(|_| "Cannot read Git status")?;
                let paths: Vec<_> = statuses
                    .iter()
                    .filter_map(|e| e.path().map(str::to_owned))
                    .filter(|p| PathBuf::from(p).parent() == Some(std::path::Path::new(notebook)))
                    .take(100)
                    .collect();
                return Ok((json!({"changed_files":paths}), None));
            }
            let mut walk = repo.revwalk().map_err(|_| "Cannot read history")?;
            walk.push_head().map_err(|_| "No Git commits yet")?;
            let commits: Vec<_> = walk
                .take(10)
                .filter_map(Result::ok)
                .filter_map(|oid| repo.find_commit(oid).ok())
                .map(|c| json!({"id":c.id().to_string(),"summary":c.summary().unwrap_or("")}))
                .collect();
            return Ok((json!({"vault_commits":commits}), None));
        }
        _ => {}
    }
    let mut before = vec![];
    if [
        "revise_entry",
        "delete_entry",
        "set_tags",
        "link_entries",
        "unlink_entries",
        "move_entry",
    ]
    .contains(&call.name.as_str())
    {
        let p = page(root, notebook, string(args, "filename")?)?;
        let raw = fs::read_to_string(p).map_err(|_| "Cannot read page")?;
        if raw.len() > 120_000 {
            return Err("Page exceeds proposal size limit".into());
        }
        before.push((string(args, "filename")?.into(), raw));
    }
    if ["link_entries", "unlink_entries"].contains(&call.name.as_str()) {
        let other = string(args, "other")?;
        if other == string(args, "filename")? {
            return Err("Cannot link a page to itself".into());
        }
        let raw =
            fs::read_to_string(page(root, notebook, other)?).map_err(|_| "Cannot read page")?;
        if raw.len() > 120_000 {
            return Err("Page exceeds proposal size limit".into());
        }
        before.push((other.into(), raw));
    }
    if ["create_entry", "revise_entry"].contains(&call.name.as_str())
        && string(args, "content")?.len() > 120_000
    {
        return Err("Page content is too long".into());
    }
    if call.name == "set_tags" {
        let tags = args["tags"].as_array().ok_or("Tags must be a list")?;
        if tags.len() > 50
            || tags
                .iter()
                .any(|v| v.as_str().is_none_or(|s| s.len() > 100))
        {
            return Err("Invalid tags".into());
        }
    }
    if call.name == "create_notebook" {
        child(&dir, string(args, "name")?)?;
    }
    if call.name == "move_entry" {
        child(&dir, string(args, "destination")?)?;
    }
    Ok((
        json!({"status":"awaiting_user_review","proposal_id":id}),
        Some(Proposal {
            id,
            name: call.name.clone(),
            arguments: args.clone(),
            before,
            applied: false,
        }),
    ))
}

fn apply_inner(root: &PathBuf, notebook: &str, proposal: &Proposal) -> Result<Value, String> {
    if proposal.applied {
        return Err("Proposal already applied".into());
    }
    for (filename, raw) in &proposal.before {
        if fs::read_to_string(page(root, notebook, filename)?).map_err(|_| "Cannot read page")?
            != *raw
        {
            return Err("Page changed since this proposal. Ask for a new proposal.".into());
        }
    }
    let args = &proposal.arguments;
    let dir = notebook_dir(root, notebook)?;
    let source = || page(root, notebook, string(args, "filename")?);
    match proposal.name.as_str() {
        "create_entry" | "revise_entry" => {
            let p = next_entry_path(
                root,
                &dir,
                &chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string(),
            );
            let raw = format!(
                "---\ncreated: {}\ntags: []\nlinks: []\n---\n\n{}\n",
                chrono::Local::now().to_rfc3339(),
                string(args, "content")?
            );
            fs::write(&p, raw).map_err(|_| "Cannot create page")?;
            if proposal.name == "revise_entry" {
                let old = source()?;
                let meta = meta_from(
                    &old,
                    &fs::read_to_string(&old).map_err(|_| "Cannot read original")?,
                );
                rewrite_meta(&p, Some(meta.tags), None)?;
                add_link(&p, old.file_stem().unwrap().to_str().unwrap())?;
                add_link(&old, p.file_stem().unwrap().to_str().unwrap())?;
            }
            Ok(json!({"filename":p.file_name().unwrap().to_string_lossy()}))
        }
        "delete_entry" => {
            let p = source()?;
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            for e in collect_entries(root) {
                if e.name != name && e.links.contains(&name) {
                    remove_link(&PathBuf::from(e.path), &name)?;
                }
            }
            fs::remove_file(p).map_err(|_| "Cannot delete page")?;
            Ok(json!({"deleted":args["filename"]}))
        }
        "set_tags" => {
            let tags = args["tags"]
                .as_array()
                .ok_or("Invalid tags")?
                .iter()
                .map(|v| v.as_str().map(clean_tag).ok_or("Invalid tag"))
                .collect::<Result<Vec<_>, _>>()?;
            rewrite_meta(&source()?, Some(dedupe(tags)), None)?;
            Ok(json!({"tags_updated":true}))
        }
        "link_entries" | "unlink_entries" => {
            let a = source()?;
            let b = page(root, notebook, string(args, "other")?)?;
            let operation = if proposal.name == "link_entries" {
                add_link
            } else {
                remove_link
            };
            operation(&a, b.file_stem().unwrap().to_str().unwrap())?;
            operation(&b, a.file_stem().unwrap().to_str().unwrap())?;
            Ok(json!({"links_updated":true}))
        }
        "create_notebook" => {
            let name = string(args, "name")?;
            child(&dir, name)?;
            create_notebook_folder(&dir, name)?;
            Ok(json!({"created":name}))
        }
        "move_entry" => {
            let name = string(args, "destination")?;
            child(&dir, name)?;
            let destination = if notebook.is_empty() {
                name.into()
            } else {
                format!("{notebook}/{name}")
            };
            move_entry_file(root, &source()?, &destination)?;
            Ok(json!({"moved_to":destination}))
        }
        "sync_vault" => Ok(json!({"sync_requested":true})),
        _ => Err("Unsupported proposal".into()),
    }
}

pub(crate) fn apply(root: &PathBuf, notebook: &str, proposal: &Proposal) -> Result<Value, String> {
    let entries = collect_entries(root);
    let backup: Vec<_> = entries
        .iter()
        .map(|e| fs::read(&e.path).map(|raw| (PathBuf::from(&e.path), raw)))
        .collect::<Result<_, _>>()
        .map_err(|_| "Cannot back up pages before applying tool")?;
    let result = apply_inner(root, notebook, proposal);
    if result.is_err() {
        for e in collect_entries(root) {
            if !backup.iter().any(|(p, _)| p == &PathBuf::from(&e.path)) {
                fs::remove_file(e.path)
                    .map_err(|_| "Tool failed and rollback could not remove a new page")?;
            }
        }
        for (p, raw) in backup {
            fs::write(p, raw).map_err(|_| "Tool failed and rollback could not restore a page")?;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn vault(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ff-tool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Ideas")).unwrap();
        fs::write(
            root.join("Ideas/a.md"),
            "---\ntags: [work]\nlinks: []\n---\n\nOriginal prose\n",
        )
        .unwrap();
        fs::write(root.join("private.md"), "Outside notebook").unwrap();
        root
    }
    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "provider-call".into(),
            name: name.into(),
            arguments: args,
        }
    }
    #[test]
    fn tools_cannot_read_or_mutate_another_notebook() {
        let root = vault("scope");
        assert!(execute(
            &root,
            "Ideas",
            &call("read_entry", json!({"filename":"../private.md"})),
            "id".into()
        )
        .is_err());
        assert!(execute(
            &root,
            "Ideas",
            &call("delete_entry", json!({"filename":"private.md"})),
            "id".into()
        )
        .is_err());
        assert!(execute(
            &root,
            "Ideas",
            &call("create_notebook", json!({"name":"../escape"})),
            "id".into()
        )
        .is_err());
        assert!(execute(&root, "Ideas", &call("shell", json!({})), "id".into()).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn revisions_are_proposed_first_and_preserve_original_prose() {
        let root = vault("revision");
        let (result, proposal) = execute(
            &root,
            "Ideas",
            &call(
                "revise_entry",
                json!({"filename":"a.md","content":"# Revised\n\nNew prose"}),
            ),
            "id".into(),
        )
        .unwrap();
        assert_eq!(result["status"], "awaiting_user_review");
        assert_eq!(collect_entries(&root).len(), 2);
        let proposal = proposal.unwrap();
        let result = apply(&root, "Ideas", &proposal).unwrap();
        let old = fs::read_to_string(root.join("Ideas/a.md")).unwrap();
        assert_eq!(split_frontmatter(&old).1, "Original prose\n");
        let new = root
            .join("Ideas")
            .join(result["filename"].as_str().unwrap());
        let meta = meta_from(&new, &fs::read_to_string(&new).unwrap());
        assert!(meta.links.contains(&"a".into()));
        assert_eq!(meta.tags, vec!["work"]);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn stale_proposals_do_not_overwrite_changed_pages() {
        let root = vault("stale");
        let (_, p) = execute(
            &root,
            "Ideas",
            &call("set_tags", json!({"filename":"a.md","tags":["new"]})),
            "id".into(),
        )
        .unwrap();
        fs::write(root.join("Ideas/a.md"), "Changed externally").unwrap();
        assert!(apply(&root, "Ideas", &p.unwrap()).is_err());
        assert_eq!(
            fs::read_to_string(root.join("Ideas/a.md")).unwrap(),
            "Changed externally"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn deleting_cleans_reciprocal_links_after_review() {
        let root = vault("delete");
        fs::write(
            root.join("Ideas/b.md"),
            "---\nlinks: [a]\n---\n\nOther prose\n",
        )
        .unwrap();
        let (_, p) = execute(
            &root,
            "Ideas",
            &call("delete_entry", json!({"filename":"a.md"})),
            "id".into(),
        )
        .unwrap();
        assert!(root.join("Ideas/a.md").exists());
        apply(&root, "Ideas", &p.unwrap()).unwrap();
        assert!(!root.join("Ideas/a.md").exists());
        let raw = fs::read_to_string(root.join("Ideas/b.md")).unwrap();
        assert!(meta_from(&root.join("Ideas/b.md"), &raw).links.is_empty());
        assert_eq!(split_frontmatter(&raw).1, "Other prose\n");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn folders_moves_links_and_tags_use_validated_operations() {
        let root = vault("organize");
        for (name, args) in [
            ("create_notebook", json!({"name":"Archive"})),
            (
                "set_tags",
                json!({"filename":"a.md","tags":["#Idea","#Idea"]}),
            ),
            (
                "create_entry",
                json!({"content":"# Table\n\n| A | B |\n|---|---|\n|1|2|"}),
            ),
        ] {
            let (_, p) = execute(&root, "Ideas", &call(name, args), "id".into()).unwrap();
            apply(&root, "Ideas", &p.unwrap()).unwrap();
        }
        let (_, p) = execute(
            &root,
            "Ideas",
            &call(
                "move_entry",
                json!({"filename":"a.md","destination":"Archive"}),
            ),
            "move".into(),
        )
        .unwrap();
        apply(&root, "Ideas", &p.unwrap()).unwrap();
        let moved = root.join("Ideas/Archive/a.md");
        assert!(moved.exists());
        let meta = meta_from(&moved, &fs::read_to_string(&moved).unwrap());
        assert_eq!(meta.tags, vec!["idea"]);
        fs::remove_dir_all(root).unwrap();
    }
}
