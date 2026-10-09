import { invoke } from "@tauri-apps/api/core";
type Server = { id: string; url: string; enabled: boolean; authenticated: boolean; tools: { name: string; description: string }[] };
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
export function setupMcp(notify: (text: string) => void) {
  const list = el<HTMLElement>("mcp-list");
  const name = el<HTMLInputElement>("mcp-name");
  const url = el<HTMLInputElement>("mcp-url");
  const token = el<HTMLInputElement>("mcp-token");
  let loading = false;
  async function refresh() {
    const servers = await invoke<Server[]>("list_mcp_servers");
    list.replaceChildren();
    if (!servers.length) { list.textContent = "No MCP servers connected."; return; }
    for (const server of servers) {
      const card = document.createElement("section");
      card.className = "mcp-server";
      const title = document.createElement("p");
      title.textContent = `${server.id} · ${server.tools.length ? `${server.tools.length} tools` : "Connect / refresh to discover tools"}${server.authenticated ? " · token saved" : ""}`;
      const endpoint = document.createElement("p"); endpoint.className = "hint"; endpoint.textContent = server.url;
      const details = document.createElement("details");
      const summary = document.createElement("summary"); summary.textContent = "Available tools"; details.append(summary);
      for (const tool of server.tools) {
        const item = document.createElement("p"); item.textContent = `${tool.name} — ${tool.description}`; details.append(item);
      }
      const edit = document.createElement("button"); edit.type = "button"; edit.className = "text-button"; edit.textContent = "Edit / refresh";
      edit.addEventListener("click", () => { name.value = server.id; url.value = server.url; token.value = ""; token.placeholder = server.authenticated ? "Leave blank to keep saved token" : "Optional bearer token"; name.focus(); });
      const toggle = document.createElement("button"); toggle.type = "button"; toggle.className = "text-button"; toggle.textContent = server.enabled ? "Disable" : "Enable";
      toggle.addEventListener("click", async () => {
        toggle.disabled = true;
        try { await invoke("enable_mcp_server", { id: server.id, enabled: !server.enabled }); await refresh(); }
        catch (e) { notify(String(e)); toggle.disabled = false; }
      });
      const remove = document.createElement("button"); remove.type = "button"; remove.className = "text-button"; remove.textContent = "Disconnect";
      remove.addEventListener("click", async () => {
        remove.disabled = true;
        try { await invoke("remove_mcp_server", { id: server.id }); await refresh(); }
        catch (e) { notify(String(e)); remove.disabled = false; }
      });
      card.append(title, endpoint, details, edit, toggle, remove); list.append(card);
    }
  }
  el<HTMLElement>("mcp-form").addEventListener("submit", async e => {
    e.preventDefault();
    if (loading) return;
    loading = true;
    const button = el<HTMLButtonElement>("mcp-connect"); button.disabled = true;
    el<HTMLElement>("mcp-status").textContent = "Connecting and discovering tools…";
    try {
      await invoke("connect_mcp_server", { server: { id: name.value.trim(), url: url.value.trim(), token: token.value.trim(), enabled: true, tools: [] } });
      token.value = "";
      await refresh();
      el<HTMLElement>("mcp-status").textContent = "Connected. External tool calls will be shown for approval.";
    } catch (e) { el<HTMLElement>("mcp-status").textContent = String(e); }
    finally { loading = false; button.disabled = false; }
  });
  return { refresh };
}
