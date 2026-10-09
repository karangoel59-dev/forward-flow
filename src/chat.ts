import { setupChatCommands, connectionId } from "./chat-commands";
import { setupMcp } from "./mcp";
import { invoke } from "@tauri-apps/api/core";
import { renderMarkdown, externalMarkdownUrl } from "./markdown";
import { openUrl } from "@tauri-apps/plugin-opener";

type Proposal = { id: string; name: string; arguments: Record<string, unknown>; before: [string, string][]; applied: boolean };
type Message = { proposals?: Proposal[]; role: "user" | "assistant"; content: string };
type Connection = { id?: string; models?: string[]; provider: string; model: string; configured: boolean; endpoint?: string };
type NotebookChat = { purpose: string; messages: Message[]; pages: number };
type Reply = { messages: Message[]; included_pages: number; total_pages: number };
const defaults: Record<string, string> = { azure_openai: "", openai: "gpt-4.1-mini", claude: "claude-sonnet-4-6", gemini: "gemini-3.8-flash" };
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

type EditorContext = {content: string; notebook: string; revision: number};
type EditorBridge = { snapshot: () => EditorContext; lock: () => boolean; unlock: () => void; replace: (content: string) => Promise<void> };
export function setupChat(close: () => void, notify: (message: string) => void, editor?: EditorBridge) {
  const mcp = setupMcp(notify);
  const provider = el<HTMLSelectElement>("chat-provider");
  const settingsConnection = el<HTMLSelectElement>("ai-connection");
  const connectionName = el<HTMLInputElement>("ai-connection-name");
  const savedModels = el<HTMLTextAreaElement>("ai-saved-models");
  const settingsProvider = el<HTMLSelectElement>("ai-provider");
  const model = el<HTMLInputElement>("ai-model");
  const endpoint = el<HTMLInputElement>("ai-endpoint");
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
  let opening = false;

  const commands = setupChatCommands({
    notebook: () => notebook,
    provider,
    connections: async () => { await loadConnections(); return connections; },
    unavailable: () => busy || saving || opening || purposeSaving,
    busy: setBusy,
    resume: async id => {
      const data = await invoke<NotebookChat>("resume_notebook_chat", {notebook, id});
      messages = data.messages; purpose.value = data.purpose; version++;
      el<HTMLElement>("chat-page-panel").hidden = true;
      el<HTMLElement>("chat-context").textContent = `${data.pages} pages in this notebook`;
      render();
    },
    newChat: startNewChat,
    refreshMcp: () => mcp.refresh(),
  });
  async function startNewChat() {
    await invoke("clear_notebook_chat", {notebook});
    messages = []; version++;
    el<HTMLElement>("chat-page-panel").hidden = true;
    render();
  }
  function updateModel() {
    const connection = connections.find(c => connectionId(c) === settingsConnection.value && c.provider === settingsProvider.value);
    model.value = connection?.model || defaults[settingsProvider.value];
    const azure = settingsProvider.value === "azure_openai";
    el<HTMLElement>("ai-endpoint-field").hidden = !azure;
    endpoint.required = azure;
    endpoint.value = connection?.endpoint || "";
    el<HTMLElement>("ai-model-label").textContent = azure ? "Azure deployment name" : "Model ID";
    model.placeholder = azure ? "Your deployment name" : "Model ID";
    savedModels.value = connection?.models?.join("\n") || connection?.model || "";
    key.value = "";
    key.placeholder = connection?.configured ? "Key saved · leave blank to keep it" : "API key";
  }
  async function loadConnections() {
    connections = await invoke<Connection[]>("get_ai_connections");
    const selected=provider.value;
    provider.replaceChildren();
    const options=[...Object.entries(defaults).map(([id])=>({id,label:id==="azure_openai"?"Azure OpenAI":id==="openai"?"OpenAI":id==="claude"?"Claude":"Gemini"})),...connections.filter(c=>connectionId(c)!==c.provider).map(c=>({id:connectionId(c),label:`${connectionId(c)} · ${c.provider}`}))];
    for(const item of options) {const option=document.createElement("option");option.value=item.id;option.textContent=item.label;provider.append(option);}
    provider.value=options.some(o=>o.id===selected)?selected:connectionId(connections.find(c=>c.configured)||connections[0]||{provider:"openai",model:"",configured:false});
    const editing=settingsConnection.value;
    settingsConnection.replaceChildren();
    const fresh=document.createElement("option");fresh.value="";fresh.textContent="New connection";settingsConnection.append(fresh);
    for(const connection of connections) {const option=document.createElement("option");option.value=connectionId(connection);option.textContent=connectionId(connection);settingsConnection.append(option);}
    settingsConnection.value=connections.some(c=>connectionId(c)===editing)?editing:"";
    updateModel(); commands.refresh(connections); return connections;
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
        details.textContent = JSON.stringify(proposal.name.endsWith("_editor") ? { content: proposal.arguments.content, notebook: (proposal.arguments.editor as EditorContext)?.notebook } : external ? { server: proposal.arguments.server, endpoint: proposal.arguments.endpoint, tool: proposal.arguments.tool, input: proposal.arguments.input } : proposal.arguments, null, 2);
        card.append(title, details);
        if (proposal.before.length) {
          const before = document.createElement("details");
          const label = document.createElement("summary");
          label.textContent = proposal.name.endsWith("_editor") ? "Original editor draft" : "Original page contents";
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
          if (saving || busy || opening || purposeSaving || proposal.applied) return;
          const editorTool = proposal.name.endsWith("_editor");
          if (editorTool && (!editor || !editor.lock())) { notify("Finish saving the editor first."); return; }
          saving = true;
          apply.disabled = true;
          try {
            const result = await invoke<Record<string, unknown>>("apply_chat_proposal", { notebook, id: proposal.id, editor: editorTool ? editor?.snapshot() : null });
            if (editorTool && typeof result.editor_content === "string") await editor!.replace(result.editor_content);
            if (external || editorTool) proposal.arguments.result = result;
            proposal.applied = true;
            render();
            notify(editorTool ? (result.action === "saved" ? "Editor saved as a page. Your draft is still in the editor." : "Editor draft updated.") : external ? "External call finished. Its result is available for your next message." : "Change applied and queued for Git backup.");
          } catch (e) { notify(String(e)); apply.disabled = false; }
          finally { saving = false; if (editorTool) editor?.unlock(); }
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
  function setBusy(value: boolean, status = "Thinking…") {
    busy = value;
    input.readOnly = value;
    el<HTMLButtonElement>("chat-send").disabled = value;
    el<HTMLButtonElement>("chat-reset").disabled = value;
    provider.disabled = value;
    el<HTMLElement>("chat-status").textContent = value ? status : "";
  }
  async function send() {
    const text = input.value.trim();
    if (busy || saving || opening || purposeSaving || !text) return;
    if (text.startsWith("/") && await commands.run(text)) return;
    commands.clear();
    if (!connections.some(c => connectionId(c) === provider.value && c.configured)) {
      document.querySelector<HTMLDetailsElement>(".chat-settings")!.open = true;
      el<HTMLDetailsElement>("ai-settings").open = true;
      const connection=connections.find(c=>connectionId(c)===provider.value);
      settingsConnection.value=connection?connectionId(connection):"";
      settingsProvider.value=connection?.provider || provider.value;
      connectionName.value=connection?connectionId(connection):provider.value;
      updateModel();
      notify("Connect this provider before chatting.");
      return;
    }
    const active = version;
    setBusy(true);
    try {
      const reply = await invoke<Reply>("chat_notebook", { notebook, provider: provider.value, message: text, editor: editor?.snapshot() || null });
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
  settingsConnection.addEventListener("change", () => {
    const connection=connections.find(c=>connectionId(c)===settingsConnection.value);
    connectionName.value=connection?connectionId(connection):"";
    if(connection) settingsProvider.value=connection.provider;
    updateModel();
  });
  settingsProvider.addEventListener("change", updateModel);
  provider.addEventListener("change", () => commands.refresh(connections));
  el<HTMLElement>("ai-form").addEventListener("submit", async e => {
    e.preventDefault();
    if (busy || saving || opening || purposeSaving) return;
    const button = el<HTMLButtonElement>("ai-save");
    saving = true;
    button.disabled = true;
    try {
      const id=connectionName.value.trim() || settingsProvider.value;
      await invoke("set_ai_connection", { id, connection: { provider: settingsProvider.value, model: model.value.trim(), models: savedModels.value.split(/[\n,]+/).map(value=>value.trim()).filter(Boolean), api_key: key.value, endpoint: settingsProvider.value === "azure_openai" ? endpoint.value.trim() : "" } });
      key.value = "";
      await loadConnections();
      provider.value = id; settingsConnection.value=id; connectionName.value=id; updateModel(); commands.refresh(connections);
      notify("AI connection saved on this device.");
    } catch (e) { notify(String(e)); }
    finally { saving = false; button.disabled = false; }
  });
  el<HTMLButtonElement>("chat-purpose-save").addEventListener("click", async () => {
    if (purposeSaving || busy || saving || opening) return;
    purposeSaving = true;
    try { await invoke("set_notebook_purpose", { notebook, purpose: purpose.value }); notify("Notebook purpose saved."); }
    catch (e) { notify(String(e)); }
    finally { purposeSaving = false; }
  });
  el<HTMLButtonElement>("chat-reset").addEventListener("click", async () => {
    if (busy || saving || opening || purposeSaving) return;
    setBusy(true, "Starting conversation…");
    try { await startNewChat(); commands.clear(); notify("New conversation. Previous chats are available with /resume."); }
    catch (e) { notify(String(e)); }
    finally { setBusy(false); }
  });
  el<HTMLButtonElement>("chat-page-cancel").addEventListener("click", () => { if (!saving) el<HTMLElement>("chat-page-panel").hidden = true; });
  el<HTMLElement>("chat-page-form").addEventListener("submit", async e => {
    e.preventDefault();
    if (saving || busy || opening || purposeSaving || !page.value.trim()) return;
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
      if (busy || saving || purposeSaving || opening) { notify("Finish the current chat or save before switching notebooks."); return false; }
      opening = true;
      try {
        const data = await invoke<NotebookChat>("get_notebook_chat", { notebook: selected });
        await loadConnections();
        notebook = selected;
        commands.clear();
        version++;
        el<HTMLElement>("chat-title").textContent = selected || "Inbox";
        el<HTMLElement>("chat-page-panel").hidden = true;
        purpose.value = data.purpose;
        messages = data.messages;
        el<HTMLElement>("chat-context").textContent = `${data.pages} pages in this notebook`;
        await mcp.refresh().catch(e => notify(String(e)));
        if (!connections.some(c => connectionId(c) === provider.value && c.configured)) provider.value = connectionId(connections.find(c=>c.configured)||connections[0]||{provider:"openai",model:"",configured:false});
        commands.refresh(connections);
        render();
        return true;
      } catch (e) { notify(String(e)); return false; }
      finally { opening = false; }
    },
  };
}
