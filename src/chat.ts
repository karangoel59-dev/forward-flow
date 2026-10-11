import { setupChatActions, type Proposal, type EditorBridge } from "./chat-actions";
import { setupChatCommands, connectionId } from "./chat-commands";
import { setupMcp } from "./mcp";
import { invoke, Channel } from "@tauri-apps/api/core";
import { renderMarkdown, externalMarkdownUrl } from "./markdown";
import { openUrl } from "@tauri-apps/plugin-opener";

type Message = { proposals?: Proposal[]; tool_pause?: string; role: "user" | "assistant"; content: string };
type Connection = { id?: string; models?: string[]; provider: string; model: string; configured: boolean; endpoint?: string };
type NotebookChat = { purpose: string; messages: Message[]; pages: number };
type Reply = { messages: Message[]; included_pages: number; total_pages: number };
type AutoApprove = { pages: boolean; editor: boolean; deletions: boolean; external: boolean; tools: string[] };
/** Automatic follow-ups allowed per message before the user must choose Continue. */
const AUTO_STEPS = 5;
type StreamEvent = { event: "round"; round: number } | { event: "text"; delta: string } | { event: "status"; message: string };
const defaults: Record<string, string> = { azure_openai: "", openai: "gpt-4.1-mini", claude: "claude-sonnet-4-6", gemini: "gemini-3.8-flash" };
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

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
  let stopping = false;
  let stopped = false;
  let turnId = "";
  let nextTurn = 0;
  const newTurn = () => { stopped = false; turnId = `${Date.now()}-${++nextTurn}-${Math.random().toString(36).slice(2)}`; return turnId; };
  let saving = false;
  let purposeSaving = false;
  let version = 0;
  let opening = false;
  let pending: { user: string; content: string } | null = null;
  let streamBody: HTMLElement | null = null;
  let streamTimer = 0;
  let autoTools: string[] = [];
  let autoSteps = 0;
  let autoSaving = false;
  const autoBoxes = [...document.querySelectorAll<HTMLInputElement>("#auto-approve-settings input[data-auto]")];

  function showAutoApprove(status?: AutoApprove | null) {
    autoTools = status?.tools ?? [];
    const on: string[] = [];
    for (const box of autoBoxes) {
      box.checked = Boolean(status?.[box.dataset.auto as "pages"]);
      if (box.checked) on.push(box.parentElement?.querySelector("span")?.firstChild?.textContent?.toLowerCase() || "");
    }
    const badge = el<HTMLElement>("chat-auto-status");
    badge.hidden = !on.length;
    badge.textContent = on.length ? `Auto-approving ${on.join(", ")}` : "";
  }
  async function loadAutoApprove() {
    try { showAutoApprove(await invoke<AutoApprove>("get_auto_approve")); }
    catch { showAutoApprove(null); }
  }
  for (const box of autoBoxes) {
    box.addEventListener("change", async () => {
      const note = el<HTMLElement>("auto-approve-status");
      if (autoSaving) { box.checked = !box.checked; return; }
      autoSaving = true;
      const settings = Object.fromEntries(autoBoxes.map(b => [b.dataset.auto, b.checked]));
      try {
        showAutoApprove(await invoke<AutoApprove>("set_auto_approve", {settings}));
        note.textContent = autoTools.length ? "Saved. These actions now run without review." : "Saved. Every action waits for your approval.";
      } catch (e) {
        // Never leave a switch showing on when the choice was not saved.
        box.checked = !box.checked; note.textContent = String(e);
        await loadAutoApprove();
      } finally { autoSaving = false; }
    });
  }
  /** Runs the newest reply's leading actions that the switches cover, then continues the chat. */
  async function autoApprove() {
    if (stopped || stopping || !autoTools.length) return;
    const latest = messages[messages.length - 1];
    const waiting = (latest?.role === "assistant" ? latest.proposals || [] : []).filter(p => !p.applied);
    // Actions run in order, so stop at the first one that still needs the user.
    const stop = waiting.findIndex(p => !autoTools.includes(p.name));
    const eligible = stop < 0 ? waiting : waiting.slice(0, stop);
    if (!eligible.length) return;
    const started = version;
    if (!await actions.run(eligible, true) || started !== version) return;
    if (eligible.length < waiting.length) return;
    if (autoSteps >= AUTO_STEPS) { notify(`Auto-approval paused after ${AUTO_STEPS} steps. Choose Continue to keep going.`); return; }
    autoSteps++;
    await send(true, true);
  }

  function flushStream() {
    streamTimer = 0;
    if (!pending || !streamBody) return;
    const follow = log.scrollHeight - log.scrollTop - log.clientHeight < 100;
    streamBody.innerHTML = renderMarkdown(pending.content);
    if (follow) log.scrollTop = log.scrollHeight;
  }

  const actions = setupChatActions({
    notebook: () => notebook,
    turn: auto => ({ turnId: auto ? turnId : newTurn(), begin: !auto }),
    unavailable: () => busy || saving || opening || purposeSaving || stopping || stopped,
    running: value => { saving = value; setBusy(value, "Running approved actions…"); if (!value) render(); },
    refreshHistory: async () => { const active = version; const data = await invoke<NotebookChat>("get_notebook_chat", {notebook}); if (active !== version) return; messages = data.messages; render(); },
    notify,
    editor,
  });
  const commands = setupChatCommands({
    notebook: () => notebook,
    provider,
    connections: async () => { await loadConnections(); return connections; },
    unavailable: () => busy || saving || opening || purposeSaving || stopping,
    busy: setBusy,
    resume: async id => adopt(await invoke<NotebookChat>("resume_notebook_chat", {notebook, id})),
    remove: async id => adopt(await invoke<NotebookChat>("delete_notebook_chat", {notebook, id})),
    newChat: startNewChat,
    refreshMcp: () => mcp.refresh(),
  });
  function adopt(data: NotebookChat) {
    turnId = "";
    stopped = data.messages[data.messages.length - 1]?.tool_pause === "stopped";
    messages = data.messages; purpose.value = data.purpose; version++; actions.reset();
    el<HTMLElement>("chat-page-panel").hidden = true;
    el<HTMLElement>("chat-context").textContent = `${data.pages} pages in this notebook`;
    render();
  }
  async function startNewChat() {
    await invoke("clear_notebook_chat", {notebook});
    turnId = ""; stopped = false; messages = []; version++; actions.reset();
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
  function render(scroll = true) {
    log.replaceChildren();
    streamBody = null;
    if (!messages.length && !pending) {
      const hint = document.createElement("p");
      hint.className = "hint";
      hint.textContent = "Explore an idea, ask about your pages, or ask for a new page. Your notebook purpose guides the conversation.";
      log.append(hint);
    }
    const visible = pending ? [...messages, { role: "user", content: pending.user }, { role: "assistant", content: pending.content }] : messages;
    visible.forEach((message, index) => {
      const streaming = Boolean(pending && index === visible.length - 1);
      const article = document.createElement("article");
      article.className = `chat-message ${message.role}${streaming ? " streaming" : ""}`;
      const label = document.createElement("p");
      label.className = "eyebrow";
      label.textContent = message.role === "user" ? "YOU" : "WRITING PARTNER";
      const body = document.createElement("div");
      body.className = "markdown-body";
      body.innerHTML = renderMarkdown(message.content);
      if (streaming) streamBody = body;
      article.append(label, body);
      if ((message as Message).tool_pause === "stopped") {
        const notice = document.createElement("p"); notice.className = "hint"; notice.textContent = "Chat stopped. Already started actions may have completed. Send a message or start a new conversation."; article.append(notice);
      }
      if (message.role === "assistant" && !streaming) {
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
      const proposals = (message as Message).proposals || [];
      if (proposals.length) {
        const group = document.createElement("details");
        group.className = "tool-group";
        const count = proposals.filter(proposal => !proposal.applied).length;
        const heading = document.createElement("summary");
        heading.textContent = count ? `${count} ${count === 1 ? "action needs" : "actions need"} approval` : `${proposals.length} ${proposals.length === 1 ? "action" : "actions"} completed`;
        group.append(heading);
        for (const proposal of proposals) group.append(actions.card(proposal));
        article.append(group);
      }
      log.append(article);
    });
    const count = actions.refresh(messages.flatMap(message => message.proposals || []));
    const latest = messages[messages.length - 1];
    const canContinue = !stopped && latest?.role === "assistant" && (Boolean(latest.tool_pause) || /Reached the tool (limit|context limit)|too many tools at once/.test(latest.content) || Boolean(latest.proposals?.length));
    el<HTMLElement>("chat-action-bar").hidden = !count && !canContinue;
    el<HTMLElement>("chat-action-count").textContent = count ? `${count} ${count === 1 ? "action needs" : "actions need"} approval` : "Ready to continue";
    el<HTMLElement>("chat-action-hint").textContent = count ? "Review and approve together." : "Use completed results in the next step.";
    el<HTMLButtonElement>("chat-review-actions").hidden = !count;
    el<HTMLButtonElement>("chat-continue").hidden = !canContinue;
    el<HTMLButtonElement>("chat-continue").disabled = busy || saving;
    if (scroll) log.scrollTop = log.scrollHeight;
  }
  function setBusy(value: boolean, status = "Thinking…") {
    busy = value;
    input.readOnly = value;
    el<HTMLButtonElement>("chat-send").disabled = value;
    el<HTMLButtonElement>("chat-reset").disabled = value;
    provider.disabled = value;
    el<HTMLButtonElement>("chat-continue").disabled = value;
    el<HTMLButtonElement>("chat-history").disabled = value;
    el<HTMLButtonElement>("chat-review-actions").disabled = value;
    el<HTMLButtonElement>("chat-options").disabled = value;
    for (const id of ["chat-stop", "chat-review-stop"]) {
      el<HTMLButtonElement>(id).disabled = stopping;
      el<HTMLButtonElement>(id).textContent = stopping ? "Stopping…" : value ? "Stop" : "End chat";
    }
    el<HTMLElement>("chat-status").textContent = stopping ? "Stopping…" : value ? status : stopped ? "Chat stopped. Send a message or start a new conversation." : "";
  }
  async function send(continuation = false, automatic = false) {
    if (!automatic) autoSteps = 0;
    let replied = false;
    const text = continuation ? "Continue my previous request using the completed tool results. Do not repeat completed actions; leave any unapproved actions pending." : input.value.trim();
    if (busy || saving || opening || purposeSaving || stopping || !text) return;
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
    if (!automatic) newTurn();
    const active = version;
    const activeTurn = turnId;
    let receiving = true;
    const onStream = new Channel<StreamEvent>();
    onStream.onmessage = event => {
      if (!receiving || active !== version || !pending) return;
      if (event.event === "round") {
        pending.content = "";
        window.clearTimeout(streamTimer); streamTimer = 0;
        flushStream();
        el<HTMLElement>("chat-status").textContent = event.round ? "Thinking with tool results…" : "Thinking…";
      } else if (event.event === "text") {
        pending.content += event.delta;
        el<HTMLElement>("chat-status").textContent = "Writing…";
        if (!streamTimer) streamTimer = window.setTimeout(flushStream, 40);
      } else if (event.event === "status") {
        el<HTMLElement>("chat-status").textContent = event.message;
      }
    };
    setBusy(true);
    try {
      const snapshot = editor?.snapshot() || null;
      pending = { user: text, content: "" };
      log.setAttribute("aria-busy", "true");
      render();
      const reply = await invoke<Reply>("chat_notebook", { notebook, provider: provider.value, message: text, editor: snapshot, onStream, turnId: activeTurn, begin: !automatic });
      if (active !== version) return;
      const follow = log.scrollHeight - log.scrollTop - log.clientHeight < 100;
      const scrollTop = log.scrollTop;
      pending = null;
      messages = reply.messages;
      if (!continuation) input.value = "";
      render(false);
      log.scrollTop = follow ? log.scrollHeight : scrollTop;
      el<HTMLElement>("chat-context").textContent = `${reply.included_pages} of ${reply.total_pages} pages included${reply.included_pages < reply.total_pages ? " · chosen by relevance to your question" : ""}`;
      replied = true;
    } catch (e) {
      if (active === version) { pending = null; render(); notify(String(e)); }
    }
    finally {
      receiving = false;
      if (active === version) {
        window.clearTimeout(streamTimer); streamTimer = 0;
        pending = null; streamBody = null;
        log.setAttribute("aria-busy", "false");
        setBusy(false);
      }
    }
    if (replied && active === version) await autoApprove();
  }
  async function stopChat() {
    if (stopping || opening || purposeSaving || (saving && !busy)) return;
    const scope = notebook;
    const id = turnId;
    version++;
    const active = version;
    stopped = true; stopping = true; autoSteps = 0;
    if (pending) {
      messages = [...messages, { role: "user", content: pending.user }, { role: "assistant", content: pending.content, tool_pause: "stopped" }];
      input.value = "";
    } else if (messages[messages.length - 1]?.role === "assistant") messages[messages.length - 1]!.tool_pause = "stopped";
    pending = null; streamBody = null;
    window.clearTimeout(streamTimer); streamTimer = 0;
    actions.reset(); saving = false;
    log.setAttribute("aria-busy", "false");
    setBusy(true); render();
    try {
      const data = await invoke<NotebookChat>("stop_notebook_chat", { notebook: scope, turnId: id });
      if (active === version && data?.messages?.length) messages = data.messages;
    } catch (error) {
      if (active === version) notify(`Could not confirm backend cancellation: ${String(error)}. Automatic actions remain stopped here; already started actions may complete.`);
    } finally {
      if (active === version) { stopping = false; setBusy(false); render(); }
    }
  }
  for (const id of ["chat-stop", "chat-review-stop"]) el<HTMLButtonElement>(id).addEventListener("click", () => void stopChat());
  el<HTMLElement>("chat-form").addEventListener("submit", e => { e.preventDefault(); send(); });
  input.addEventListener("keydown", e => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); send(); }
  });
  el<HTMLButtonElement>("chat-close").addEventListener("click", close);
  el<HTMLButtonElement>("chat-continue").addEventListener("click", () => void send(true));
  el<HTMLButtonElement>("chat-history").addEventListener("click", async () => {
    const draft = input.value;
    try { await commands.run("/resume"); }
    finally { input.value = draft; }
  });
  const options = el<HTMLDetailsElement>("chat-options-panel");
  el<HTMLButtonElement>("chat-options").addEventListener("click", () => { options.open = !options.open; });
  options.addEventListener("toggle", () => { el<HTMLElement>("chat-options").setAttribute("aria-expanded", String(options.open)); });
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
    if (busy || saving || opening || purposeSaving || stopping) return;
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
    if (busy || saving || opening || purposeSaving || stopping) return;
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
        // Not awaited: a slow settings read must never hold up opening the chat.
        void loadAutoApprove();
        notebook = selected;
        turnId = "";
        actions.reset();
        commands.clear();
        version++;
        el<HTMLElement>("chat-title").textContent = selected || "Inbox";
        el<HTMLElement>("chat-page-panel").hidden = true;
        purpose.value = data.purpose;
        messages = data.messages;
        stopped = messages[messages.length - 1]?.tool_pause === "stopped";
        setBusy(false);
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
