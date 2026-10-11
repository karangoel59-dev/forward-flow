import { invoke } from "@tauri-apps/api/core";

type Keep = "mine" | "theirs" | "both";
type ConflictFile = { path: string; mine: string | null; theirs: string | null; both: "combine" | "copy" | null };
type Conflicts = { local: string; remote: string; files: ConflictFile[] };
const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export function setupConflicts(notify: (text: string) => void, open: () => void, close: () => void) {
  const banner = el<HTMLButtonElement>("conflict-banner");
  const list = el<HTMLElement>("conflict-list");
  const status = el<HTMLElement>("conflict-status");
  const apply = el<HTMLButtonElement>("conflict-apply");
  const cancel = el<HTMLButtonElement>("conflict-cancel");
  let pending: Conflicts | null = null;
  let choices = new Map<string, Keep>();
  let busy = false;

  function update() {
    const total = pending?.files.length ?? 0;
    apply.disabled = busy || !total || choices.size < total;
    if (total && !busy) status.textContent = `${choices.size} of ${total} chosen`;
  }
  function version(title: string, content: string | null, missing: string) {
    const column = document.createElement("div"); column.className = "conflict-version";
    const heading = document.createElement("h3"); heading.textContent = title;
    const body = document.createElement("pre"); body.textContent = content ?? missing;
    if (content === null) body.className = "hint";
    column.append(heading, body);
    return column;
  }
  function render() {
    list.replaceChildren();
    (pending?.files ?? []).forEach((file, index) => {
      const card = document.createElement("fieldset"); card.className = "conflict-file";
      const name = document.createElement("legend"); name.textContent = file.path;
      const versions = document.createElement("div"); versions.className = "conflict-versions";
      versions.append(
        version("This device", file.mine, "Deleted on this device"),
        version("Other device", file.theirs, "Deleted on the other device"),
      );
      const options = document.createElement("div"); options.className = "conflict-options";
      const offered: [Keep, string][] = [
        ["mine", file.mine === null ? "Keep it deleted" : "Keep this device"],
        ["theirs", file.theirs === null ? "Delete it here too" : "Keep other device"],
      ];
      if (file.both) offered.push(["both", file.both === "combine" ? "Keep both · combine tags and links" : "Keep both · save the other as a copy"]);
      for (const [keep, text] of offered) {
        const label = document.createElement("label");
        const radio = document.createElement("input");
        radio.type = "radio"; radio.name = `conflict-${index}`; radio.value = keep;
        radio.checked = choices.get(file.path) === keep;
        radio.addEventListener("change", () => { choices.set(file.path, keep); update(); });
        label.append(radio, ` ${text}`);
        options.append(label);
      }
      card.append(name, versions, options);
      list.append(card);
    });
    update();
  }
  function setBusy(value: boolean, text = "") {
    busy = value; cancel.disabled = value;
    list.querySelectorAll<HTMLInputElement>("input").forEach(input => { input.disabled = value; });
    if (text) status.textContent = text;
    update();
  }

  /** Opens the review screen. Returns false when there turned out to be nothing to resolve. */
  async function review() {
    if (busy) return true;
    pending = null; choices = new Map(); render();
    open();
    setBusy(true, "Checking both versions…");
    try {
      pending = await invoke<Conflicts>("get_conflicts");
    } catch (e) {
      setBusy(false); status.textContent = String(e);
      return true;
    }
    setBusy(false);
    if (!pending.files.length) {
      banner.hidden = true; pending = null; close();
      notify("no sync conflicts · syncing…");
      invoke("resync_vault").catch(() => {});
      return false;
    }
    render();
    return true;
  }

  banner.addEventListener("click", () => { void review(); });
  cancel.addEventListener("click", () => { if (!busy) close(); });
  apply.addEventListener("click", async () => {
    if (busy || !pending || choices.size < pending.files.length) return;
    const { local, remote, files } = pending;
    setBusy(true, "Resolving…");
    try {
      await invoke("resolve_conflicts", { local, remote, choices: files.map(file => ({ path: file.path, keep: choices.get(file.path) })) });
      pending = null; choices = new Map(); banner.hidden = true;
      setBusy(false); close();
      notify("conflict resolved · syncing…");
    } catch (e) {
      setBusy(false); status.textContent = String(e);
    }
  });

  return {
    review,
    /** Sync found files changed on both devices; offer the review without interrupting writing. */
    flag(count: number) {
      banner.textContent = `${count === 1 ? "1 page" : `${count} pages`} changed on two devices · Review`;
      banner.hidden = false;
    },
    clear() { banner.hidden = true; },
  };
}
