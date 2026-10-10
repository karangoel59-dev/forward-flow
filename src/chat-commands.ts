import { invoke } from "@tauri-apps/api/core";

export type ChatConnection = { id?: string; provider: string; model: string; models?: string[]; configured: boolean; endpoint?: string };
type Session = { id: string; title: string; updated_at: string; provider?: string; model?: string; messages: number; active: boolean };
type Server = { id: string; url: string; enabled: boolean; tool_discovery?: boolean; tools: unknown[] };
type ModelList = { models: string[]; note: string };
type Row = { label: string; detail?: string; run?: () => Promise<void> };
type Dependencies = {
  notebook: () => string;
  provider: HTMLSelectElement;
  connections: () => Promise<ChatConnection[]>;
  unavailable: () => boolean;
  busy: (value: boolean, status?: string) => void;
  resume: (id: string) => Promise<void>;
  newChat: () => Promise<void>;
  refreshMcp: () => Promise<void>;
};
const labels: Record<string, string> = { openai: "OpenAI", azure_openai: "Azure OpenAI", claude: "Claude", gemini: "Gemini" };
export const connectionId = (connection: ChatConnection) => connection.id || connection.provider;
const connectionName = (connection: ChatConnection) => connectionId(connection) === connection.provider ? labels[connection.provider] || connection.provider : `${connectionId(connection)} (${labels[connection.provider] || connection.provider})`;
const commands = [
  { name: "/provider", detail: "List connections or /provider <provider>" },
  { name: "/model", detail: "List/switch models, /model saved, /model add <name>" },
  { name: "/mcp", detail: "Show servers or /mcp reconnect <server>" },
  { name: "/resume", detail: "Resume a saved chat in this notebook" },
  { name: "/new", detail: "Start a new chat; keep the current conversation" },
  { name: "/help", detail: "Show chat commands" },
];
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
export function setupChatCommands(deps: Dependencies) {
  const input = el<HTMLTextAreaElement>("chat-input");
  const panel = el<HTMLElement>("chat-command-panel");
  const title = el<HTMLElement>("chat-command-title");
  const results = el<HTMLElement>("chat-command-results");
  const suggestions = el<HTMLElement>("chat-command-suggestions");
  let localBusy = false;
  let selection = 0;
  let matches = commands;
  async function perform(run: () => Promise<void>) {
    if (localBusy || deps.unavailable()) return;
    localBusy = true; deps.busy(true, "Running command…");
    panel.setAttribute("aria-busy", "true");
    try { await run(); }
    catch (error) { show("Command failed", String(error)); }
    finally { localBusy = false; deps.busy(false); panel.setAttribute("aria-busy", "false"); }
  }
  function show(heading: string, note: string, rows: Row[] = []) {
    title.textContent = heading; results.replaceChildren(); panel.hidden = false;
    const hint = document.createElement("p"); hint.className = "hint"; hint.textContent = note; results.append(hint);
    for (const row of rows) {
      const item = document.createElement(row.run ? "button" : "div");
      item.className = "chat-command-row";
      if (item instanceof HTMLButtonElement) {
        item.type = "button";
        item.addEventListener("click", () => { if (row.run) void perform(row.run); });
      }
      const name = document.createElement("span"); name.textContent = row.label; item.append(name);
      if (row.detail) { const detail = document.createElement("span"); detail.className = "hint"; detail.textContent = row.detail; item.append(detail); }
      results.append(item);
    }
  }
  function selectProvider(id: string, connections: ChatConnection[]) {
    const connection=connections.find(c => connectionId(c) === id);
    if (!connection?.configured) throw new Error(`Connect ${labels[id] || id} in Purpose & connections first.`);
    deps.provider.value = id;
    updateBadge(connections);
  }
  async function providers(args: string[]) {
    const connections = await deps.connections();
    const id = args[0] === "change" || args[0] === "set" ? args[1] : args[0];
    if ((args[0] === "change" || args[0] === "set") && !id) throw new Error("Use /provider <connection name>.");
    if (id && id !== "list") {
      if (args.length > ((args[0] === "change" || args[0] === "set") ? 2 : 1)) throw new Error("Use /provider <connection name>.");
      selectProvider(id, connections);
      const connection=connections.find(c=>connectionId(c)===id)!;
      show("Provider changed", `${connectionName(connection)} · ${connection.model}`);
      return;
    }
    if (args.length > 1) throw new Error("Use /provider list or /provider <connection name>.");
    const available=connections.concat(Object.keys(labels).filter(id=>!connections.some(c=>connectionId(c)===id)).map(provider=>({provider,model:"",configured:false})));
    show("Providers", "Choose a named connection for your next message.", available.map(connection => {
      const id=connectionId(connection);
      return {label:`${connectionName(connection)}${deps.provider.value===id?" · current":""}`,detail:connection.configured?`${connection.model} · ${new Set([connection.model,...connection.models || []]).size} saved models`:"Not connected",run:connection.configured?async()=>{selectProvider(id,connections);show("Provider changed",`${connectionName(connection)} · ${connection.model}`);}:undefined};
    }));
  }
  async function changeModel(provider: string, model: string) {
    if (!(await deps.connections()).some(c => connectionId(c) === provider && c.configured)) throw new Error("Connect this provider first.");
    await invoke("set_ai_model", {provider, model});
    const connections = await deps.connections(); selectProvider(provider, connections);
    show("Model changed", `${provider} · ${model}`);
    updateBadge(connections);
  }
  async function models(args: string[]) {
    const id=deps.provider.value;
    const connections=await deps.connections();
    const connection=connections.find(c=>connectionId(c)===id);
    if (!connection?.configured) throw new Error("Connect this provider first.");
    const saved=[...new Set([connection.model,...connection.models || []])];
    const rows=(models:string[])=>models.map(model=>({label:model,detail:connection.model===model?"Current · saved":saved.includes(model)?"Saved model":undefined,run:()=>changeModel(id,model)}));
    if (args[0] === "add" || args[0] === "save") {
      if (args.length!==2) throw new Error("Use /model add <model or deployment>.");
      await invoke("save_ai_model",{provider:id,model:args[1]});await deps.connections();
      show("Model saved",`${args[1]} saved for ${connectionName(connection)}. Current model stays ${connection.model}.`);return;
    }
    if (args[0] && !["list","saved","available"].includes(args[0])) {
      const model=args[0]==="change" || args[0]==="set"?args[1]:args[0];
      if (!model || args.length>((args[0]==="change" || args[0]==="set")?2:1)) throw new Error("Use /model <model or deployment>.");
      await changeModel(id,model);return;
    }
    if (args.length>1) throw new Error("Use /model saved, /model available, or /model <model>.");
    if (args[0]==="saved") {show(`${connectionName(connection)} models`,"Saved models for quick switching.",rows(saved));return;}
    show(`${connectionName(connection)} models`,"Saved models. Loading provider catalog…",rows(saved));
    try {
      const list=await invoke<ModelList>("list_ai_models",{provider:id});
      show(`${connectionName(connection)} models`,list.note,rows([...new Set([...saved,...list.models])]));
    } catch(error) {show(`${connectionName(connection)} models`,`${String(error)} Saved models remain available; use /model <model> for a known model.`,rows(saved));}
  }
  async function reconnect(id: string) {
    await invoke("reconnect_mcp_server", {id});
    await deps.refreshMcp();
  }
  async function mcps(args: string[]) {
    const servers = await invoke<Server[]>("list_mcp_servers");
    if (args[0] === "reconnect") {
      const id = args[1];
      if (!id || args.length !== 2) throw new Error("Use /mcp reconnect <server> or /mcp reconnect all.");
      const targets = id === "all" ? servers : servers.filter(s => s.id === id);
      if (!targets.length) throw new Error("No matching MCP server. Use /mcp to list saved servers.");
      const rows: Row[] = [];
      for (const server of targets) {
        try { await reconnect(server.id); rows.push({label:server.id,detail:"Reconnected and tools refreshed"}); }
        catch (error) { rows.push({label:server.id,detail:String(error)}); }
      }
      show("MCP reconnect", "Server enable/disable settings were preserved.", rows); return;
    }
    if (args[0] === "enable" || args[0] === "disable") {
      if (args.length !== 2 || !servers.some(s => s.id === args[1])) throw new Error("Use /mcp enable <server> or /mcp disable <server>.");
      await invoke("enable_mcp_server", {id:args[1],enabled:args[0] === "enable"});
      await deps.refreshMcp(); show("MCP updated",`${args[1]} ${args[0]}d.`); return;
    }
    if (args.length && (!["list", "show"].includes(args[0]) || args.length !== 1)) throw new Error("Use /mcp, /mcp reconnect <server>, or /mcp enable|disable <server>.");
    show("MCP servers", servers.length ? "Saved servers and discovered tools. Select a server to reconnect and refresh its tools." : "No MCP servers saved. Add one under Purpose & connections.", servers.map(server => ({label:server.id,detail:`${server.enabled ? "Enabled" : "Disabled"} · ${server.tools.length} tools${server.enabled && server.tools.length ? (server.tool_discovery ? " · Available through discovery" : " · Available to chat") : ""} · ${server.url}`,run:async () => {await reconnect(server.id);show("MCP reconnected",`${server.id} tools refreshed.`);}})));
  }
  async function resume(args: string[]) {
    const sessions = await invoke<Session[]>("list_notebook_chats", {notebook:deps.notebook()});
    if (args.length > 1) throw new Error("Use /resume or /resume <conversation ID>.");
    if (args[0] && args[0] !== "list") {
      const session = args[0] === "latest" ? sessions.find(s => !s.active) : sessions.find(s => s.id === args[0]);
      if (!session) throw new Error("Conversation not found in this notebook.");
      await deps.resume(session.id); show("Conversation resumed", session.title); return;
    }
    show("Saved conversations", sessions.length ? "Choose a conversation to continue. Your current conversation and editor draft are kept." : "No saved conversations in this notebook yet.", sessions.map(session => ({label:`${session.title}${session.active ? " · current" : ""}`,detail:`${session.messages} messages${session.provider ? ` · ${labels[session.provider] || session.provider}${session.model ? ` / ${session.model}` : ""}` : ""} · ${new Date(session.updated_at).toLocaleString()} · ${session.id}`,run:async () => {await deps.resume(session.id);show("Conversation resumed",session.title);}})));
  }
  function updateBadge(connections: ChatConnection[]) {
    const connection = connections.find(c => connectionId(c) === deps.provider.value);
    el<HTMLElement>("chat-model-current").textContent = connection?.model || "Connect a provider";
  }
  function suggest() {
    const value = input.value;
    matches = /^\/[^\s]*$/.test(value) ? commands.filter(c => c.name.startsWith(value.toLowerCase())) : [];
    suggestions.replaceChildren(); suggestions.hidden = !matches.length;
    input.setAttribute("aria-expanded", String(!!matches.length));
    if (selection >= matches.length) selection = 0;
    matches.forEach((command,index) => {
      const button = document.createElement("button"); button.type = "button"; button.className = "chat-command-suggestion";
      button.id = `chat-suggestion-${index}`; button.setAttribute("role", "option"); button.setAttribute("aria-selected",String(index === selection));
      button.textContent = `${command.name} — ${command.detail}`;
      button.addEventListener("click", () => { input.value = `${command.name} `; suggest(); input.focus(); });
      suggestions.append(button);
    });
    if (matches.length) input.setAttribute("aria-activedescendant",`chat-suggestion-${selection}`); else input.removeAttribute("aria-activedescendant");
  }
  input.addEventListener("input", () => {selection = 0; suggest();});
  input.addEventListener("keydown", event => {
    if (suggestions.hidden) return;
    if (event.key === "Escape") { event.preventDefault(); suggestions.hidden = true; input.setAttribute("aria-expanded","false"); input.removeAttribute("aria-activedescendant"); }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault(); selection = (selection + (event.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length; suggest();
    }
    if ((event.key === "Tab" || (event.key === "Enter" && !event.metaKey && !event.ctrlKey && !event.shiftKey)) && matches.length) {event.preventDefault();input.value = `${matches[selection].name} `;suggest();}
  });
  el<HTMLButtonElement>("chat-command-close").addEventListener("click", () => {panel.hidden = true;});
  return {
    refresh: updateBadge,
    clear: () => {panel.hidden = true;suggestions.hidden = true;input.setAttribute("aria-expanded","false");input.removeAttribute("aria-activedescendant");},
    async run(text: string) {
      if (!text.startsWith("/")) return false;
      if (localBusy || deps.unavailable()) return true;
      suggestions.hidden = true;
      input.setAttribute("aria-expanded", "false"); input.removeAttribute("aria-activedescendant");
      await perform(async () => {
        const [command,...args] = text.split(/\s+/);
        switch (command.toLowerCase()) {
          case "/provider": await providers(args); break;
          case "/model": await models(args); break;
          case "/mcp": await mcps(args); break;
          case "/resume": await resume(args); break;
          case "/new": if(args.length) throw new Error("Use /new."); await deps.newChat(); show("New conversation", "Previous conversations are available with /resume."); break;
          case "/help": show("Chat commands", "Commands run locally and are never sent to the model.", commands.map(c => ({label:c.name,detail:c.detail}))); break;
          default: throw new Error("Unknown command. Use /help for available commands.");
        }
        input.value = "";
        updateBadge(await deps.connections());
      });
      return true;
    },
  };
}
