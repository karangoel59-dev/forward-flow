//! Which chat tool proposals may run without the user reviewing each one.

use serde::{Deserialize, Serialize};

const PAGES: &[&str] = &[
    "create_entry",
    "revise_entry",
    "set_tags",
    "link_entries",
    "unlink_entries",
    "create_notebook",
    "move_entry",
    "sync_vault",
];
const EDITOR: &[&str] = &["replace_editor", "save_editor", "clear_editor"];
const DELETIONS: &[&str] = &["delete_entry"];
const EXTERNAL: &[&str] = &["mcp_call"];

/// Everything is off until the user opts in, one kind of action at a time.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AutoApprove {
    /// Changes recorded in the vault's Git history.
    pub pages: bool,
    /// Changes to the unsaved draft, which has no history.
    pub editor: bool,
    pub deletions: bool,
    /// Calls that leave the device and may act on other services.
    pub external: bool,
}

impl AutoApprove {
    /// Proposal names that may be applied without review.
    pub(crate) fn tools(&self) -> Vec<&'static str> {
        [
            (self.pages, PAGES),
            (self.editor, EDITOR),
            (self.deletions, DELETIONS),
            (self.external, EXTERNAL),
        ]
        .into_iter()
        .filter(|(enabled, _)| *enabled)
        .flat_map(|(_, names)| names.iter().copied())
        .collect()
    }

    /// Unknown or future tools always need review.
    pub(crate) fn allows(&self, tool: &str) -> bool {
        self.tools().contains(&tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_auto_approved_until_a_category_is_switched_on() {
        let off = AutoApprove::default();
        assert!(off.tools().is_empty());
        for tool in ["create_entry", "delete_entry", "replace_editor", "mcp_call"] {
            assert!(!off.allows(tool));
        }
        let missing: AutoApprove = serde_json::from_str("{}").unwrap();
        assert_eq!(missing, off);
    }

    #[test]
    fn each_switch_covers_only_its_own_actions() {
        let pages = AutoApprove { pages: true, ..Default::default() };
        assert!(pages.allows("create_entry") && pages.allows("set_tags") && pages.allows("sync_vault"));
        assert!(!pages.allows("delete_entry"), "deleting needs its own switch");
        assert!(!pages.allows("clear_editor") && !pages.allows("mcp_call"));

        let editor = AutoApprove { editor: true, ..Default::default() };
        assert_eq!(editor.tools(), EDITOR);
        let external = AutoApprove { external: true, ..Default::default() };
        assert_eq!(external.tools(), ["mcp_call"]);
        let everything = AutoApprove { pages: true, editor: true, deletions: true, external: true };
        assert!(!everything.allows("shell") && !everything.allows(""));
    }

    #[test]
    fn every_tool_that_proposes_a_change_belongs_to_exactly_one_switch() {
        let everything = AutoApprove { pages: true, editor: true, deletions: true, external: true };
        let known = everything.tools();
        let read_only = ["list_entries", "search_entries", "read_entry", "git_status", "git_history", "read_editor"];
        for definition in crate::ai::tools::definitions().into_iter().chain(crate::ai::editor::definitions()) {
            let name = definition["name"].as_str().unwrap().to_owned();
            assert_ne!(read_only.contains(&name.as_str()), known.contains(&name.as_str()), "{name}");
        }
        let mut unique = known.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), known.len());
    }
}
