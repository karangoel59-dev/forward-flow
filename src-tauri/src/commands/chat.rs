use crate::{
    ai::{self, Connection, ConnectionStatus, Message},
    config::vault_dir,
    entries::{collect_entries, split_frontmatter},
    notebooks::{notebook_dir, read_purpose, write_purpose},
    state::VAULT_WRITES,
    vault_git,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};

static CHAT_BUSY: AtomicBool = AtomicBool::new(false);
struct ChatGuard;
impl Drop for ChatGuard {
    fn drop(&mut self) {
        CHAT_BUSY.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize)]
pub(crate) struct NotebookChat {
    pub purpose: String,
    pub messages: Vec<Message>,
    pub pages: usize,
}
#[derive(Serialize)]
pub(crate) struct ChatReply {
    pub messages: Vec<Message>,
    pub included_pages: usize,
    pub total_pages: usize,
}

#[tauri::command]
pub(crate) fn get_ai_connections(app: AppHandle) -> Result<Vec<ConnectionStatus>, String> {
    Ok(ai::connections(&app)?
        .values()
        .map(|c| ConnectionStatus {
            provider: c.provider.clone(),
            model: c.model.clone(),
            configured: !c.api_key.is_empty(),
        })
        .collect())
}
#[tauri::command]
pub(crate) fn set_ai_connection(app: AppHandle, connection: Connection) -> Result<(), String> {
    ai::save_connection(&app, connection)
}
#[tauri::command]
pub(crate) fn get_notebook_chat(app: AppHandle, notebook: String) -> Result<NotebookChat, String> {
    let root = vault_dir(&app)?;
    let dir = notebook_dir(&root, &notebook)?;
    Ok(NotebookChat {
        purpose: read_purpose(&dir)?,
        messages: ai::history(&app, &dir)?,
        pages: collect_entries(&root)
            .iter()
            .filter(|e| e.notebook == notebook)
            .count(),
    })
}
#[tauri::command]
pub(crate) fn set_notebook_purpose(
    app: AppHandle,
    notebook: String,
    purpose: String,
) -> Result<(), String> {
    let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let root = vault_dir(&app)?;
    write_purpose(&notebook_dir(&root, &notebook)?, &purpose)?;
    vault_git::record(
        &app,
        root,
        format!(
            "Set purpose for notebook {}",
            if notebook.is_empty() {
                "Inbox"
            } else {
                &notebook
            }
        ),
        true,
    );
    Ok(())
}
#[tauri::command]
pub(crate) fn clear_notebook_chat(app: AppHandle, notebook: String) -> Result<(), String> {
    if CHAT_BUSY.load(Ordering::SeqCst) {
        return Err("Wait for the current reply before starting a new chat".into());
    }
    ai::save_history(&app, &notebook_dir(&vault_dir(&app)?, &notebook)?, &[])
}

fn context(root: &PathBuf, notebook: &str) -> Result<(Vec<Value>, usize), String> {
    let entries: Vec<_> = collect_entries(root)
        .into_iter()
        .filter(|e| e.notebook == notebook)
        .collect();
    let total = entries.len();
    let mut pages = vec![];
    let mut remaining = 60_000;
    for entry in entries {
        if fs::metadata(&entry.path)
            .map_err(|_| "Cannot read page metadata")?
            .len()
            > (remaining + 4096) as u64
        {
            continue;
        }
        let raw = fs::read_to_string(&entry.path).map_err(|_| "Cannot read a notebook page")?;
        let (_, body) = split_frontmatter(&raw);
        if body.len() > remaining {
            continue;
        }
        remaining -= body.len();
        pages.push(json!({"filename":format!("{}.md",entry.name),"body":body}));
    }
    Ok((pages, total))
}

#[tauri::command]
pub(crate) async fn chat_notebook(
    app: AppHandle,
    notebook: String,
    provider: String,
    message: String,
) -> Result<ChatReply, String> {
    if CHAT_BUSY.swap(true, Ordering::SeqCst) {
        return Err("A chat reply is already in progress".into());
    }
    let _guard = ChatGuard;
    let (dir, mut messages, system, included_pages, total_pages, connection) = {
        let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let root = vault_dir(&app)?;
        let dir = notebook_dir(&root, &notebook)?;
        let connection = ai::connections(&app)?
            .remove(&provider)
            .ok_or("Connect this provider in AI settings first")?;
        let mut messages = ai::history(&app, &dir)?;
        messages.push(Message {
            proposals: vec![],
            role: "user".into(),
            content: message.trim().into(),
        });
        ai::validate_messages(&messages)?;
        let (pages, total) = context(&root, &notebook)?;
        let system = ai::system_prompt(&read_purpose(&dir)?, &pages);
        (dir, messages, system, pages.len(), total, connection)
    };
    let root = vault_dir(&app)?;
    let (url, mut body) = ai::providers::request(&connection, &system, &messages)?;
    let mcp_servers = crate::mcp::servers(&app)?;
    let mut definitions = ai::tools::definitions();
    definitions.extend(crate::mcp::definitions(&mcp_servers));
    ai::providers::attach_tools(&provider, &mut body, &definitions);
    let mut proposals = vec![];
    let mut activity = vec![];
    let mut reply = String::new();
    for round in 0..6 {
        let data = match ai::exchange(&connection, &url, &body).await {
            Ok(data) => data,
            Err(error) if round > 0 => {
                reply = format!(
                    "{error}\n\nTool activity completed: {}. Review any proposed changes below.",
                    activity.join(", ")
                );
                break;
            }
            Err(error) => return Err(error),
        };
        let calls = match ai::providers::calls(&provider, &data) {
            Ok(calls) => calls,
            Err(error) => {
                reply = format!("{error}. Review proposals already prepared below.");
                break;
            }
        };
        if calls.is_empty() {
            reply = ai::providers::text(&provider, &data)
                .unwrap_or_else(|error| format!("{error}. Review any proposals below."));
            break;
        }
        if calls.len() > 8 {
            reply =
                "The model requested too many tools at once. Review proposals already prepared."
                    .into();
            break;
        }
        let mut results = vec![];
        for call in calls {
            let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
            let id = format!(
                "{}-{}-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                round,
                proposals.len()
            );
            let outcome = if call.name.starts_with("mcp_") {
                crate::mcp::proposal(&mcp_servers, &call, id).map(|p| {
                    (
                        json!({"status":"awaiting_user_review","proposal_id":p.id}),
                        Some(p),
                    )
                })
            } else {
                ai::tools::execute(&root, &notebook, &call, id)
            };
            let result = match outcome {
                Ok((result, proposal)) => {
                    if let Some(p) = proposal {
                        proposals.push(p);
                    }
                    result
                }
                Err(error) => json!({"error":error}),
            };
            activity.push(call.name.clone());
            results.push((call, result));
        }
        ai::providers::append_results(&provider, &mut body, &data, &results)?;
        if body.to_string().len() > 350_000 {
            reply =
                "Reached the tool context limit. Review proposals or ask a more focused question."
                    .into();
            break;
        }
    }
    if reply.is_empty() {
        reply="Reached the tool limit for this turn. Review the proposals below or ask a follow-up question.".into();
    }
    if !activity.is_empty() {
        reply.push_str(&format!("\n\nTools used: {}.", activity.join(", ")));
    }

    messages.push(Message {
        role: "assistant".into(),
        content: reply,
        proposals,
    });
    ai::save_history(&app, &dir, &messages)?;
    Ok(ChatReply {
        messages,
        included_pages,
        total_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_is_limited_to_the_selected_notebook() {
        let root = std::env::temp_dir().join(format!("ff-chat-context-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Ideas/Nested")).unwrap();
        fs::write(root.join("inbox.md"), "private inbox").unwrap();
        fs::write(root.join("Ideas/a.md"), "selected page").unwrap();
        fs::write(root.join("Ideas/Nested/b.md"), "different notebook").unwrap();
        fs::write(root.join("Ideas/huge.md"), "x".repeat(60_001)).unwrap();
        let (pages, total) = context(&root, "Ideas").unwrap();
        assert_eq!(total, 2);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["body"], "selected page");
        fs::remove_dir_all(root).unwrap();
    }
}

#[tauri::command]
pub(crate) async fn apply_chat_proposal(
    app: AppHandle,
    notebook: String,
    id: String,
) -> Result<Value, String> {
    if CHAT_BUSY.swap(true, Ordering::SeqCst) {
        return Err("Wait for the current chat reply".into());
    }
    let _guard = ChatGuard;
    let root = vault_dir(&app)?;
    let dir = notebook_dir(&root, &notebook)?;
    let external = {
        let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let mut messages = ai::history(&app, &dir)?;
        let proposal = messages
            .iter_mut()
            .flat_map(|m| m.proposals.iter_mut())
            .find(|p| p.id == id)
            .ok_or("Proposal is no longer available")?;
        if proposal.applied {
            return Err("This proposal was already applied or attempted".into());
        }
        if proposal.name == "mcp_call" {
            // Persist consumption before sending: a network failure does not prove a remote action failed.
            proposal.applied = true;
            let external = proposal.clone();
            ai::save_history(&app, &dir, &messages)?;
            Some(external)
        } else {
            let result = ai::tools::apply(&root, &notebook, proposal)?;
            proposal.applied = true;
            let name = proposal.name.clone();
            ai::save_history(&app, &dir, &messages)?;
            if name == "sync_vault" {
                vault_git::sync_now(&app, root.clone(), true);
            } else {
                vault_git::record(&app, root.clone(), format!("Chat tool {name}"), true);
            }
            let _ = app.emit("vault-updated", ());
            return Ok(result);
        }
    };
    let proposal = external.unwrap();
    let result=crate::mcp::execute(&app,&proposal).await.unwrap_or_else(|error|json!({"error":error,"notice":"The remote action may have completed. Check the server before requesting another call."}));
    {
        let _write = VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let mut messages = ai::history(&app, &dir)?;
        let proposal = messages
            .iter_mut()
            .flat_map(|m| m.proposals.iter_mut())
            .find(|p| p.id == id)
            .ok_or("Proposal history changed")?;
        proposal.arguments["result"] = result.clone();
        ai::save_history(&app, &dir, &messages)?;
    }
    Ok(result)
}
