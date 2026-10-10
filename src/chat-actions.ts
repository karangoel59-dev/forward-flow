import { invoke } from "@tauri-apps/api/core";

export type Proposal = { id: string; name: string; arguments: Record<string, unknown>; before: [string, string][]; applied: boolean };
export type EditorContext = { content: string; notebook: string; revision: number };
export type EditorBridge = { snapshot: () => EditorContext; lock: () => boolean; unlock: () => void; replace: (content: string) => Promise<void> };
type Options = {
  notebook: () => string;
  unavailable: () => boolean;
  running: (value: boolean) => void;
  refreshHistory: () => Promise<void>;
  notify: (message: string) => void;
  editor?: EditorBridge;
};
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const labels: Record<string, string> = {
  create_entry: "Create page", revise_entry: "Revise page", delete_entry: "Delete page",
  move_entry: "Move page", create_notebook: "Create notebook", set_tags: "Update tags",
  link_entries: "Link pages", unlink_entries: "Unlink pages", sync_vault: "Sync notebook",
  replace_editor: "Edit draft", clear_editor: "Clear draft", save_editor: "Save draft as page",
};
function title(proposal: Proposal) {
  return proposal.name === "mcp_call" ? String(proposal.arguments.tool || "External tool") : labels[proposal.name] || proposal.name.split("_").join(" ");
}
function summary(proposal: Proposal) {
  const args = proposal.arguments;
  if (proposal.name === "mcp_call") return `${args.server} · external call`;
  if (proposal.name.endsWith("_editor")) return `Current draft · ${(args.editor as EditorContext)?.notebook || "Inbox"}`;
  return String(args.filename || args.name || args.notebook || (typeof args.content === "string" ? `${args.content.length.toLocaleString()} characters` : "Notebook change"));
}
function failed(proposal: Proposal) {
  const result = proposal.arguments.result as Record<string, unknown> | undefined;
  return Boolean(result?.error || result?.isError);
}
export function setupChatActions(options: Options) {
  const dialog = el<HTMLDialogElement>("chat-review");
  const list = el<HTMLElement>("chat-review-list");
  const selectAll = el<HTMLInputElement>("chat-select-all");
  const approve = el<HTMLButtonElement>("chat-approve-selected");
  const status = el<HTMLElement>("chat-review-status");
  const selected = new Set<string>();
  let proposals: Proposal[] = [];
  let running = false;
  const pending = () => proposals.filter(p => !p.applied);

  function updateSelection() {
    const available = pending();
    const count = available.filter(p => selected.has(p.id)).length;
    selectAll.checked = available.length > 0 && count === available.length;
    selectAll.indeterminate = count > 0 && count < available.length;
    selectAll.disabled = running || !available.length;
    approve.disabled = running || !count;
    approve.textContent = count ? `Approve & run ${count} selected` : "Approve selected";
    el<HTMLElement>("chat-selection-count").textContent = `${count} selected`;
  }
  function details(proposal: Proposal) {
    const section = document.createElement("details");
    section.className = "tool-details";
    const heading = document.createElement("summary");
    heading.textContent = "Review details";
    const args = proposal.arguments;
    const data = proposal.name.endsWith("_editor") ? { content: args.content, notebook: (args.editor as EditorContext)?.notebook } : proposal.name === "mcp_call" ? { server: args.server, endpoint: args.endpoint, tool: args.tool, input: args.input } : Object.fromEntries(Object.entries(args).filter(([key]) => key !== "result"));
    const contents = document.createElement("pre");
    contents.textContent = JSON.stringify(data, null, 2);
    section.append(heading, contents);
    if (proposal.before.length) {
      const before = document.createElement("details");
      const label = document.createElement("summary");
      label.textContent = proposal.name.endsWith("_editor") ? "Original editor draft" : "Original page contents";
      before.append(label);
      for (const [filename, content] of proposal.before) {
        const source = document.createElement("pre"); source.textContent = `${filename}\n${content}`; before.append(source);
      }
      section.append(before);
    }
    return section;
  }
  function card(proposal: Proposal, selectable = false) {
    const section = document.createElement("section");
    section.className = `tool-proposal${proposal.applied ? " completed" : ""}${failed(proposal) ? " failed" : ""}`;
    const header = document.createElement("div"); header.className = "tool-proposal-head";
    const label = document.createElement("label"); label.className = "tool-proposal-label";
    if (selectable && !proposal.applied) {
      const checkbox = document.createElement("input"); checkbox.type = "checkbox"; checkbox.checked = selected.has(proposal.id); checkbox.disabled = running;
      checkbox.setAttribute("aria-label", `Approve ${title(proposal)}: ${summary(proposal)}`);
      checkbox.addEventListener("change", () => { if (checkbox.checked) selected.add(proposal.id); else selected.delete(proposal.id); updateSelection(); });
      label.append(checkbox);
    }
    const text = document.createElement("span");
    const name = document.createElement("strong"); name.textContent = title(proposal);
    const hint = document.createElement("span"); hint.className = "hint"; hint.textContent = summary(proposal);
    text.append(name, hint); label.append(text);
    const state = document.createElement("span"); state.className = "tool-state";
    state.textContent = failed(proposal) ? "Needs attention" : proposal.applied ? "Completed" : "Needs approval";
    header.append(label, state); section.append(header, details(proposal));
    if (!selectable || proposal.applied) {
      const apply = document.createElement("button"); apply.type = "button"; apply.className = "text-button tool-apply";
      apply.textContent = proposal.applied ? (proposal.name === "mcp_call" ? "Attempted" : "Applied") : (proposal.name === "mcp_call" ? "Approve & run external tool" : "Apply change");
      apply.disabled = running || proposal.applied;
      apply.addEventListener("click", () => void run([proposal]));
      section.append(apply);
    }
    if (proposal.arguments.result) {
      const output = document.createElement("details"); const heading = document.createElement("summary"); heading.textContent = "Result";
      const contents = document.createElement("pre"); contents.textContent = JSON.stringify(proposal.arguments.result, null, 2);
      output.append(heading, contents); section.append(output);
    }
    return section;
  }
  function renderReview() {
    list.replaceChildren(...proposals.map(p => card(p, true)));
    if (!proposals.length) { const text = document.createElement("p"); text.className = "hint"; text.textContent = "No tool actions in this conversation yet."; list.append(text); }
    updateSelection();
  }
  function refresh(next: Proposal[]) {
    proposals = next;
    for (const id of selected) if (!pending().some(p => p.id === id)) selected.delete(id);
    const count = pending().length;
    const badge = el<HTMLElement>("nav-chat-count"); badge.hidden = count === 0; badge.textContent = String(count);
    renderReview();
    return count;
  }
  async function run(items: Proposal[]) {
    if (running || options.unavailable()) return;
    const queue = items.filter(p => !p.applied);
    if (!queue.length) return;
    running = true; options.running(true); renderReview();
    el<HTMLButtonElement>("chat-review-close").disabled = true;
    let completed = 0;
    try {
      for (const proposal of queue) {
        status.textContent = `Running ${completed + 1} of ${queue.length}: ${title(proposal)}…`;
        const editorTool = proposal.name.endsWith("_editor");
        if (editorTool && (!options.editor || !options.editor.lock())) throw new Error("Finish saving the editor first.");
        try {
          const result = await invoke<Record<string, unknown>>("apply_chat_proposal", { notebook: options.notebook(), id: proposal.id, editor: editorTool ? options.editor?.snapshot() : null });
          proposal.applied = true; proposal.arguments.result = result; selected.delete(proposal.id);
          if (editorTool && typeof result.editor_content === "string") await options.editor!.replace(result.editor_content);
          completed++;
          renderReview();
          if (result.error || result.isError) throw new Error(`${title(proposal)} reported an error. Review its result before continuing.`);
        } finally { if (editorTool) options.editor?.unlock(); }
      }
      status.textContent = `${completed} ${completed === 1 ? "action" : "actions"} completed. Continue the conversation to use the results.`;
      options.notify(status.textContent);
    } catch (error) {
      status.textContent = `${String(error)} Stopped; remaining actions were not run.`;
      options.notify(status.textContent);
    } finally {
      try { await options.refreshHistory(); }
      catch {
        for (const proposal of queue) { proposal.applied = true; selected.delete(proposal.id); }
        status.textContent = "Could not refresh action status. Reopen the conversation before retrying.";
        options.notify(status.textContent);
      }
      running = false; options.running(false); renderReview();
      el<HTMLButtonElement>("chat-review-close").disabled = false;
    }
  }
  el<HTMLButtonElement>("chat-review-actions").addEventListener("click", () => { status.textContent = ""; renderReview(); dialog.showModal(); });
  el<HTMLButtonElement>("chat-review-close").addEventListener("click", () => { if (!running) dialog.close(); });
  dialog.addEventListener("cancel", event => { if (running) event.preventDefault(); });
  selectAll.addEventListener("change", () => { for (const p of pending()) { if (selectAll.checked) selected.add(p.id); else selected.delete(p.id); } renderReview(); });
  approve.addEventListener("click", () => void run(pending().filter(p => selected.has(p.id))));
  return { card, refresh, reset: () => { selected.clear(); status.textContent = ""; dialog.close(); } };
}
