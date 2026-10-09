import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
export function setupSettings(imported: () => Promise<void>, notify: (text: string) => void) {
  const file = el<HTMLInputElement>("config-file");
  const status = el<HTMLElement>("config-status");
  const apply = el<HTMLButtonElement>("config-apply");
  const cancel = el<HTMLButtonElement>("config-cancel");
  const panel = el<HTMLElement>("config-review");
  let pending = "";
  let busy = false;
  function clear() { pending = ""; file.value = ""; panel.hidden = true; }
  for (const id of ["setup-import-config", "settings-import-config"]) {
    el<HTMLButtonElement>(id).addEventListener("click", () => { if (!busy) file.click(); });
  }
  file.addEventListener("change", async () => {
    const selected = file.files?.[0];
    clear();
    if (!selected || busy) return;
    try {
      if (selected.size > 1_000_000) throw new Error("Config file exceeds 1 MB.");
      const text = await selected.text();
      const config = JSON.parse(text);
      if (config.version !== 1) throw new Error("Unsupported config version.");
      const models = Object.keys(config.models || {});
      const servers = Object.keys(config.mcp_servers || {});
      status.textContent = `${models.length} model connections, ${servers.length} MCP servers${config.git?.remote ? ", and Git remote settings" : ""}. Matching connections will be updated; other connections and your vault folder will be kept.`;
      pending = text;
      panel.hidden = false;
    } catch (e) { notify(String(e)); }
  });
  cancel.addEventListener("click", clear);
  apply.addEventListener("click", async () => {
    if (!pending || busy) return;
    busy = true; apply.disabled = true; cancel.disabled = true;
    try {
      const result = await invoke<string>("import_settings", { text: pending });
      clear();
      await imported();
      notify(result);
    } catch (e) { notify(String(e)); }
    finally { busy = false; apply.disabled = false; cancel.disabled = false; }
  });
  el<HTMLButtonElement>("settings-export-config").addEventListener("click", async () => {
    if (busy) return;
    busy = true;
    try {
      const path = await save({ defaultPath: "forward-flow-config.json", filters: [{ name: "JSON config", extensions: ["json"] }] });
      if (path) {
        await invoke("export_settings", { path, includeSecrets: el<HTMLInputElement>("config-secrets").checked });
        notify("Config exported.");
      }
    } catch (e) { notify(String(e)); }
    finally { busy = false; }
  });
}
