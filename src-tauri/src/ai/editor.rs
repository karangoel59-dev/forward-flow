use super::{providers::ToolCall, tools::Proposal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct EditorContext {
    pub content: String,
    pub notebook: String,
    pub revision: u64,
}
pub(crate) fn definitions() -> Vec<Value> {
    [("read_editor", "Read the current unsaved editor draft"),
     ("replace_editor", "Propose replacing or formatting the editor draft with Markdown"),
     ("save_editor", "Propose saving the current draft as a notebook page; keep draft in editor"),
     ("clear_editor", "Propose clearing the unsaved editor draft")]
        .into_iter().map(|(name, description)| json!({"name":name,"description":description,"parameters":{"type":"object","properties":if name == "replace_editor" {json!({"content":{"type":"string"}})} else {json!({})},"required":if name == "replace_editor" {vec!["content"]} else {vec![]},"additionalProperties":false}})).collect()
}
pub(crate) fn execute(
    context: &EditorContext,
    call: &ToolCall,
    id: String,
) -> Result<(Value, Option<Proposal>), String> {
    let args = call
        .arguments
        .as_object()
        .ok_or("Invalid editor arguments")?;
    if !definitions().iter().any(|d| d["name"] == call.name) {
        return Err("Unknown editor tool".into());
    }
    if args
        .keys()
        .any(|k| call.name != "replace_editor" || k != "content")
    {
        return Err("Unknown editor argument".into());
    }
    if context.content.len() > 120_000 {
        return Err("Editor draft exceeds 120 KB".into());
    }
    if call.name == "read_editor" {
        return Ok((
            json!({"content":context.content,"notebook":context.notebook,"unsaved":true}),
            None,
        ));
    }
    if call.name == "save_editor" && context.content.trim().is_empty() {
        return Err("Editor is empty".into());
    }
    let content = if call.name == "replace_editor" {
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or("Provide replacement Markdown content")?;
        if content.len() > 120_000 {
            return Err("Replacement exceeds 120 KB".into());
        }
        Some(content)
    } else {
        None
    };
    let proposal = Proposal {
        id,
        name: call.name.clone(),
        arguments: json!({"editor":context,"content":content}),
        before: vec![("Unsaved editor draft".into(), context.content.clone())],
        applied: false,
    };
    Ok((
        json!({"status":"awaiting_user_review","proposal_id":proposal.id}),
        Some(proposal),
    ))
}
pub(crate) fn validate(proposal: &Proposal, current: &EditorContext) -> Result<(), String> {
    let expected: EditorContext = serde_json::from_value(proposal.arguments["editor"].clone())
        .map_err(|_| "Invalid editor proposal")?;
    if expected.content != current.content
        || expected.notebook != current.notebook
        || expected.revision != current.revision
    {
        return Err("Editor changed since this proposal. Ask for a new proposal.".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_reads_and_proposes_without_mutating_and_rejects_stale_drafts() {
        let context = EditorContext {
            content: "draft".into(),
            notebook: "Ideas".into(),
            revision: 1,
        };
        let call = ToolCall {
            id: "call".into(),
            name: "read_editor".into(),
            arguments: json!({}),
        };
        assert_eq!(
            execute(&context, &call, "id".into()).unwrap().0["content"],
            "draft"
        );
        let call = ToolCall {
            name: "replace_editor".into(),
            arguments: json!({"content":"# Draft"}),
            ..call
        };
        let p = execute(&context, &call, "id".into()).unwrap().1.unwrap();
        assert!(!p.applied);
        assert!(validate(&p, &context).is_ok());
        assert!(validate(
            &p,
            &EditorContext {
                revision: 2,
                ..context
            }
        )
        .is_err());
    }
}
