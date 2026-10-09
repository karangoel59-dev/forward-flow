import { setupMcp } from "./mcp";
import { invoke } from "@tauri-apps/api/core";
import { renderMarkdown, externalMarkdownUrl } from "./markdown";
import { openUrl } from "@tauri-apps/plugin-opener";

type Proposal = { id: string; name: string; arguments: Record<string, unknown>; before: [string, string][]; applied: boolean };
type Message = { proposals?: Proposal[]; role: "user" | "assistant"; content: string };
type Connection = { provider: string; model: string; configured: boolean };
type NotebookChat = { purpose: string; messages: Message[]; pages: number };
type Reply = { messages: Message[]; included_pages: number; total_pages: number };
const defaults: Record<string, string> = { openai: "gpt-4.1-mini", claude: "claude-sonnet-4-6", gemini: "gemini-2.5-flash" };
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export function setupChat(close: () => void, notify: (message: string) => void) {
  const mcp = setupMcp(notify);
  const provider = el<HTMLSelectElement>("chat-provider");
  const settingsProvider = el<HTMLSelectElement>("ai-provider");
  const model = el<HTMLInputElement>("ai-model");
  const key = el<HTMLInputElement>("ai-key");
  const input = el<HTMLTextAreaElement>("chat-input");
  const purpose = el<HTMLTextAreaElement>("chat-purpose");
  const log = el<HTMLElement>("chat-log");
  const page = el<HTMLTextAreaElement>("chat-page");
  let notebook = "";
  let connections: Connection[] = [];
  let messages: Message[] = [];
  let busy = false;
  let saving = false;
  let purposeSaving = false;
  let version = 0;

  function updateModel() {
    const connection = connections.find(c => c.provider === settingsProvider.value);
    model.value = connection?.model || defaults[settingsProvider.value];
    key.value = "";
    key.placeholder = connection?.configured ? "Key saved · leave blank to keep it" : "API key";
  }
  async function loadConnections() {
    connections = await invoke<Connection[]>("get_ai_connections");
    updateModel();
  }
  function render() {
    log.replaceChildren();
    if (!messages.length) {
      const hint = document.createElement("p");
      hint.className = "hint";
      hint.textContent = "Explore an idea, ask about your pages, or ask for a new page. Your notebook purpose guides the conversation.";
      log.append(hint);
    }
    messages.forEach(message => {
      const article = document.createElement("article");
      article.className = `chat-message ${message.role}`;
      const label = document.createElement("p");
      label.className = "eyebrow";
      label.textContent = message.role === "user" ? "YOU" : "WRITING PARTNER";
      const body = document.createElement("div");
      body.className = "markdown-body";
      body.innerHTML = renderMarkdown(message.content);
      article.append(label, body);
      if (message.role === "assistant") {
        const button = document.createElement("button");
        button.className = "text-button";
        button.textContent = "Save as page ↗";
        button.type = "button";
        button.addEventListener("click", () => {
          page.value = message.content;
          el<HTMLElement>("chat-page-panel").hidden = false;
          page.focus();
        });
        article.append(button);
      }
      for (const proposal of message.proposals || []) {
        const card = document.createElement("section");
        card.className = "tool-proposal";
        const title = document.createElement("p");
        title.className = "eyebrow";
        title.textContent = proposal.name.split("_").join(" ");
        const details = document.createElement("pre");
        const external = proposal.name === "mcp_call";
        details.textContent = JSON.stringify(external ? { server: proposal.arguments.server, endpoint: proposal.arguments.endpoint, tool: proposal.arguments.tool, input: proposal.arguments.input } : proposal.arguments, null, 2);
        card.append(title, details);
        if (proposal.before.length) {
          const before = document.createElement("details");
          const label = document.createElement("summary");
          label.textContent = "Original page contents";
          before.append(label);
          for (const [filename, contents] of proposal.before) {
            const source = document.createElement("pre");
            source.textContent = `${filename}\n${contents}`;
            before.append(source);
          }
          card.append(before);
        }
        const apply = document.createElement("button");
        apply.type = "button";
        apply.className = "ghost-btn";
        apply.textContent = proposal.applied ? (external ? "Attempted" : "Applied") : (external ? "Approve & run external tool" : "Apply change");
        apply.disabled = proposal.applied;
        apply.addEventListener("click", async () => {
          if (saving || busy || proposal.applied) return;
          saving = true;
          apply.disabled = true;
          try {
            const result = await invoke<Record<string, unknown>>("apply_chat_proposal", { notebook, id: proposal.id });
            if (external) proposal.arguments.result = result;
            proposal.applied = true;
            render();
            notify(external ? "External call finished. Its result is available for your next message." : "Change applied and queued for Git backup.");
          } catch (e) { notify(String(e)); apply.disabled = false; }
          finally { saving = false; }
        });
        card.append(apply);
        if (external && proposal.arguments.result) {
          const output = document.createElement("pre");
          output.textContent = JSON.stringify(proposal.arguments.result, null, 2);
          card.append(output);
        }
        article.append(card);
      }
      log.append(article);
    });
    log.scrollTop = log.scrollHeight;
  }
  function setBusy(value: boolean) {
    busy = value;
    input.readOnly = value;
    el<HTMLButtonElement>("chat-send").disabled = value;
    el<HTMLButtonElement>("chat-reset").disabled = value;
    provider.disabled = value;
    el<HTMLElement>("chat-status").textContent = value ? "Thinking…" : "";
  }
  async function send() {
    const text = input.value.trim();
    if (busy || saving || !text) return;
    if (!connections.some(c => c.provider === provider.value && c.configured)) {
      el<HTMLDetailsElement>("ai-settings").open = true;
      settingsProvider.value = provider.value;
      updateModel();
      notify("Connect this provider before chatting.");
      return;
    }
    const active = version;
    setBusy(true);
    try {
      const reply = await invoke<Reply>("chat_notebook", { notebook, provider: provider.value, message: text });
      if (active !== version) return;
      messages = reply.messages;
      input.value = "";
      render();
      const latest = log.lastElementChild as HTMLElement | null;
      if (latest) log.scrollTop = latest.offsetTop - log.offsetTop;
      el<HTMLElement>("chat-context").textContent = `${reply.included_pages} of ${reply.total_pages} pages included${reply.included_pages < reply.total_pages ? " · some pages exceeded the context limit" : ""}`;
    } catch (e) { if (active === version) notify(String(e)); }
    finally { setBusy(false); }
  }
  el<HTMLElement>("chat-form").addEventListener("submit", e => { e.preventDefault(); send(); });
  input.addEventListener("keydown", e => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); send(); }
  });
  el<HTMLButtonElement>("chat-close").addEventListener("click", close);
  settingsProvider.addEventListener("change", updateModel);
  el<HTMLElement>("ai-form").addEventListener("submit", async e => {
    e.preventDefault();
    const button = el<HTMLButtonElement>("ai-save");
    button.disabled = true;
    try {
      await invoke("set_ai_connection", { connection: { provider: settingsProvider.value, model: model.value.trim(), api_key: key.value } });
      provider.value = settingsProvider.value;
      key.value = "";
      await loadConnections();
      notify("AI connection saved on this device.");
    } catch (e) { notify(String(e)); }
    finally { button.disabled = false; }
  });
  el<HTMLButtonElement>("chat-purpose-save").addEventListener("click", async () => {
    if (purposeSaving) return;
    purposeSaving = true;
    try { await invoke("set_notebook_purpose", { notebook, purpose: purpose.value }); notify("Notebook purpose saved."); }
    catch (e) { notify(String(e)); }
    finally { purposeSaving = false; }
  });
  el<HTMLButtonElement>("chat-reset").addEventListener("click", async () => {
    if (busy || saving) return;
    try { await invoke("clear_notebook_chat", { notebook }); messages = []; render(); }
    catch (e) { notify(String(e)); }
  });
  el<HTMLButtonElement>("chat-page-cancel").addEventListener("click", () => { if (!saving) el<HTMLElement>("chat-page-panel").hidden = true; });
  el<HTMLElement>("chat-page-form").addEventListener("submit", async e => {
    e.preventDefault();
    if (saving || !page.value.trim()) return;
    saving = true;
    const button = el<HTMLButtonElement>("chat-page-save");
    button.disabled = true;
    try {
      await invoke("save_chat_page", { notebook, content: page.value });
      el<HTMLElement>("chat-page-panel").hidden = true;
      notify(`Page saved in ${notebook || "Inbox"}.`);
    } catch (e) { notify(String(e)); }
    finally { saving = false; button.disabled = false; }
  });
  log.addEventListener("click", e => {
    const anchor = (e.target as Element).closest<HTMLAnchorElement>("a[href]");
    if (!anchor) return;
    e.preventDefault();
    const url = externalMarkdownUrl(anchor.getAttribute("href") || "");
    if (url) openUrl(url).catch(e => notify(String(e)));
  });
  return {
    async open(selected: string) {
      if (busy || saving || purposeSaving) { notify("Finish the current chat or save before switching notebooks."); return false; }
      notebook = selected;
      version++;
      el<HTMLElement>("chat-title").textContent = selected || "Inbox";
      el<HTMLElement>("chat-page-panel").hidden = true;
      try {
        const data = await invoke<NotebookChat>("get_notebook_chat", { notebook });
        purpose.value = data.purpose;
        messages = data.messages;
        el<HTMLElement>("chat-context").textContent = `${data.pages} pages in this notebook`;
        await loadConnections();
        await mcp.refresh().catch(e => notify(String(e)));
        if (!connections.some(c => c.provider === provider.value)) provider.value = connections[0]?.provider || "openai";
        render();
        return true;
      } catch (e) { notify(String(e)); return false; }
    },
  };
}
