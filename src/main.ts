import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";

type EntryMeta = {
  path: string;
  name: string;
  created: string;
  words: number;
  preview: string;
  tags: string[];
  links: string[];
};

type EntryFull = { meta: EntryMeta; body: string; related: EntryMeta[] };
type Mode = "setup" | "write" | "reader" | "picker";
type Sort = "new" | "old" | "long" | "linked";

const SORTS: Sort[] = ["new", "old", "long", "linked"];
const SORT_LABEL: Record<Sort, string> = {
  new: "newest first",
  old: "oldest first",
  long: "longest first",
  linked: "most linked",
};

const el = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const setup = el<HTMLElement>("setup");
const writeView = el<HTMLElement>("write");
const editor = el<HTMLTextAreaElement>("editor");

const reader = el<HTMLElement>("reader");
const readerDate = el<HTMLElement>("reader-date");
const readerCount = el<HTMLElement>("reader-count");
const readerBody = el<HTMLElement>("reader-body");
const readerTags = el<HTMLElement>("reader-tags");
const tagInput = el<HTMLInputElement>("tag-input");
const relatedBox = el<HTMLElement>("reader-related");
const relatedList = el<HTMLUListElement>("related-list");

const picker = el<HTMLElement>("picker");
const pickerHead = el<HTMLElement>("picker-head");
const pickerFilter = el<HTMLInputElement>("picker-filter");
const pickerList = el<HTMLUListElement>("picker-list");
const sortLabel = el<HTMLElement>("sort-label");
const hudEl = el<HTMLElement>("hud");

const win = getCurrentWindow();

let mode: Mode = "write";
let entries: EntryMeta[] = [];
let shown: EntryMeta[] = [];
let cursor = 0;
let sort: Sort = "new";

let current: EntryFull | null = null;
let relCursor = 0;
let tagEditing = false;
let readerOrigin: Mode = "write";
let linkFor: string | null = null;
let committing = false;

// ---------------------------------------------------------------- hud

let hudTimer = 0;
function hud(msg: string, ms = 1900) {
  hudEl.textContent = msg;
  hudEl.classList.add("show");
  clearTimeout(hudTimer);
  hudTimer = window.setTimeout(() => hudEl.classList.remove("show"), ms);
}

// ------------------------------------------------- autosizing type

const MAX_PX = 34;
const MIN_PX = 17;
let rafId = 0;

/** Largest font size (within bounds) at which the draft still fits unscrolled. */
function fit() {
  editor.style.fontSize = `${MAX_PX}px`;
  if (editor.scrollHeight <= editor.clientHeight) return;

  let lo = MIN_PX;
  let hi = MAX_PX;
  let best = MIN_PX;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    editor.style.fontSize = `${mid}px`;
    if (editor.scrollHeight <= editor.clientHeight) {
      best = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  editor.style.fontSize = `${best}px`;

  // Past the floor the page scrolls instead; keep the live line in view.
  if (best === MIN_PX && editor.scrollHeight > editor.clientHeight) {
    editor.scrollTop = editor.scrollHeight;
  }
}

function scheduleFit() {
  if (rafId) return;
  rafId = requestAnimationFrame(() => {
    rafId = 0;
    fit();
  });
}

// ---------------------------------------------------------------- draft

let draftTimer = 0;
function scheduleDraftSave() {
  clearTimeout(draftTimer);
  draftTimer = window.setTimeout(() => {
    invoke("save_draft", { content: editor.value }).catch(() => {});
  }, 700);
}

// ---------------------------------------------------------------- modes

function show(next: Mode) {
  mode = next;
  setup.hidden = next !== "setup";
  reader.hidden = next !== "reader";
  picker.hidden = next !== "picker";
  writeView.style.visibility = next === "write" ? "visible" : "hidden";

  if (next === "write") {
    editor.focus();
    scheduleFit();
  } else if (next === "picker") {
    pickerFilter.focus();
  } else {
    (document.activeElement as HTMLElement | null)?.blur();
  }
}

function formatDate(raw: string) {
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) return raw;
  return d.toLocaleString(undefined, { dateStyle: "long", timeStyle: "short" });
}

// ---------------------------------------------------------------- commit

async function commit() {
  if (committing) return;
  if (!editor.value.trim()) {
    hud("nothing written yet");
    return;
  }
  committing = true;
  try {
    const meta = await invoke<EntryMeta>("commit_entry", { content: editor.value });
    editor.classList.add("committing");
    window.setTimeout(() => {
      editor.value = "";
      editor.classList.remove("committing");
      fit();
      editor.focus();
      committing = false;
    }, 260);
    entries = [];
    hud(`locked · ${meta.words} ${meta.words === 1 ? "word" : "words"}`);
  } catch (e) {
    committing = false;
    hud(String(e));
  }
}

// ---------------------------------------------------------------- reader

function renderTags() {
  readerTags.replaceChildren();
  if (!current) return;
  for (const tag of current.meta.tags) {
    const chip = document.createElement("span");
    chip.className = "tag";
    chip.textContent = tag;
    readerTags.append(chip);
  }
}

function renderRelated() {
  relatedList.replaceChildren();
  const items = current?.related ?? [];
  relatedBox.hidden = items.length === 0;
  if (!items.length) return;

  if (relCursor >= items.length) relCursor = items.length - 1;
  if (relCursor < 0) relCursor = 0;

  items.forEach((entry, i) => {
    const li = document.createElement("li");
    if (i === relCursor) li.className = "on";

    const top = document.createElement("div");
    top.className = "row-top";
    const when = document.createElement("span");
    when.textContent = formatDate(entry.created);
    const count = document.createElement("span");
    count.textContent = `${entry.words}w`;
    top.append(when, count);

    const preview = document.createElement("div");
    preview.className = "row-preview";
    preview.textContent = entry.preview || "—";

    li.append(top, preview);
    li.addEventListener("click", () => openEntry(entry.path));
    relatedList.append(li);
  });
}

async function openEntry(path: string, remember = true) {
  if (remember && mode === "picker") readerOrigin = "picker";
  else if (remember && mode === "write") readerOrigin = "write";
  try {
    const full = await invoke<EntryFull>("read_entry", { path });
    current = full;
    relCursor = 0;
    tagEditing = false;
    tagInput.hidden = true;
    readerDate.textContent = formatDate(full.meta.created);
    readerCount.textContent = `${full.meta.words} words`;
    readerBody.textContent = full.body.trim();
    renderTags();
    renderRelated();
    reader.querySelector<HTMLElement>(".reader-inner")!.scrollTop = 0;
    show("reader");
  } catch (e) {
    hud(String(e));
  }
}

// ---------------------------------------------------------------- tagging

function beginTagEdit() {
  if (!current) return;
  tagInput.value = current.meta.tags.join(", ");
  tagInput.hidden = false;
  tagEditing = true;
  tagInput.focus();
  tagInput.select();
}

function endTagEdit() {
  tagEditing = false;
  tagInput.hidden = true;
  tagInput.blur();
}

async function saveTags() {
  if (!current) return;
  const tags = tagInput.value
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);
  const path = current.meta.path;
  try {
    await invoke<EntryMeta>("set_tags", { path, tags });
    endTagEdit();
    await openEntry(path, false);
    hud(tags.length ? "tags saved" : "tags cleared");
  } catch (e) {
    hud(String(e));
  }
}

// ---------------------------------------------------------------- linking

async function beginLink() {
  if (!current) return;
  linkFor = current.meta.path;
  await openPicker();
}

async function chooseInPicker(entry: EntryMeta) {
  if (!linkFor) {
    openEntry(entry.path);
    return;
  }
  const from = linkFor;
  linkFor = null;
  try {
    await invoke("link_entries", { a: from, b: entry.path });
    hud("linked");
  } catch (e) {
    hud(String(e));
  }
  await openEntry(from, false);
}

async function unlinkSelected() {
  if (!current) return;
  const target = current.related[relCursor];
  if (!target) return;
  const path = current.meta.path;
  try {
    await invoke("unlink_entries", { a: path, b: target.path });
    await openEntry(path, false);
    hud("unlinked");
  } catch (e) {
    hud(String(e));
  }
}

// ---------------------------------------------------------------- picker

function sortEntries(list: EntryMeta[]): EntryMeta[] {
  const out = list.slice();
  switch (sort) {
    case "new":
      return out.sort((a, b) => b.name.localeCompare(a.name));
    case "old":
      return out.sort((a, b) => a.name.localeCompare(b.name));
    case "long":
      return out.sort((a, b) => b.words - a.words);
    case "linked":
      return out.sort(
        (a, b) => b.links.length - a.links.length || b.name.localeCompare(a.name),
      );
  }
}

function renderPicker() {
  const raw = pickerFilter.value.trim().toLowerCase();
  let pool = entries;

  // In link mode you cannot link an entry to itself.
  if (linkFor) pool = pool.filter((e) => e.path !== linkFor);

  if (raw.startsWith("#")) {
    const want = raw.slice(1);
    pool = want
      ? pool.filter((e) => e.tags.some((t) => t.includes(want)))
      : pool.filter((e) => e.tags.length > 0);
  } else if (raw) {
    pool = pool.filter((e) =>
      `${e.name} ${e.preview} ${e.tags.join(" ")}`.toLowerCase().includes(raw),
    );
  }

  shown = sortEntries(pool);
  sortLabel.textContent = SORT_LABEL[sort];

  if (cursor >= shown.length) cursor = Math.max(0, shown.length - 1);
  pickerList.replaceChildren();

  if (!shown.length) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = entries.length ? "no match" : "nothing written yet";
    pickerList.append(li);
    return;
  }

  shown.forEach((entry, i) => {
    const li = document.createElement("li");
    if (i === cursor) li.className = "on";

    const top = document.createElement("div");
    top.className = "row-top";
    const when = document.createElement("span");
    when.textContent = formatDate(entry.created);
    const count = document.createElement("span");
    count.textContent = entry.links.length
      ? `${entry.words}w · ${entry.links.length} linked`
      : `${entry.words}w`;
    if (entry.links.length) count.className = "row-link-count";
    top.append(when, count);

    const preview = document.createElement("div");
    preview.className = "row-preview";
    preview.textContent = entry.preview || "—";
    li.append(top, preview);

    if (entry.tags.length) {
      const tags = document.createElement("div");
      tags.className = "row-tags";
      for (const tag of entry.tags) {
        const t = document.createElement("span");
        t.className = "row-tag";
        t.textContent = `#${tag}`;
        tags.append(t);
      }
      li.append(tags);
    }

    li.addEventListener("click", () => chooseInPicker(entry));
    pickerList.append(li);
  });

  pickerList.children[cursor]?.scrollIntoView({ block: "nearest" });
}

async function openPicker() {
  try {
    entries = await invoke<EntryMeta[]>("list_entries");
  } catch (e) {
    hud(String(e));
    return;
  }
  pickerHead.hidden = !linkFor;
  pickerHead.textContent = linkFor ? "Link to which entry?" : "";
  pickerFilter.value = "";
  cursor = 0;
  renderPicker();
  show("picker");
}

function movePicker(delta: number) {
  if (!shown.length) return;
  cursor = Math.min(shown.length - 1, Math.max(0, cursor + delta));
  renderPicker();
}

function cycleSort() {
  sort = SORTS[(SORTS.indexOf(sort) + 1) % SORTS.length];
  cursor = 0;
  renderPicker();
  hud(SORT_LABEL[sort]);
}

// ---------------------------------------------------------------- vault

async function chooseVault() {
  const picked = await open({
    directory: true,
    multiple: false,
    title: "Choose a folder for your entries",
  });
  if (typeof picked !== "string") return;
  try {
    await invoke("set_vault", { path: picked });
    entries = [];
    hud("folder set");
    show("write");
  } catch (e) {
    hud(String(e));
  }
}

// ---------------------------------------------------------------- keys

async function toggleFullscreen() {
  try {
    await win.setFullscreen(!(await win.isFullscreen()));
  } catch {
    /* window may not allow it; nothing to report */
  }
}

tagInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    saveTags();
  } else if (e.key === "Escape") {
    e.preventDefault();
    endTagEdit();
  }
  e.stopPropagation();
});

document.addEventListener("keydown", (e) => {
  const mod = e.metaKey || e.ctrlKey;
  const typing =
    e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;

  if (e.metaKey && e.ctrlKey && e.key.toLowerCase() === "f") {
    e.preventDefault();
    toggleFullscreen();
    return;
  }

  if (mod && e.shiftKey && e.key.toLowerCase() === "o") {
    e.preventDefault();
    chooseVault();
    return;
  }

  if (mode === "setup") return;

  if (mod && e.key.toLowerCase() === "o") {
    e.preventDefault();
    if (mode === "picker") {
      linkFor = null;
      show("write");
    } else {
      linkFor = null;
      openPicker();
    }
    return;
  }

  if (mode === "write") {
    if (mod && (e.key === "Enter" || e.key.toLowerCase() === "s")) {
      e.preventDefault();
      commit();
    }
    return;
  }

  // ------------------------------------------------------------ reader
  if (mode === "reader") {
    if (tagEditing) return;

    if (e.key === "Escape") {
      e.preventDefault();
      readerOrigin === "picker" ? openPicker() : show("write");
      return;
    }
    if (typing) return;

    const key = e.key.toLowerCase();
    if (key === "t") {
      e.preventDefault();
      beginTagEdit();
    } else if (key === "l") {
      e.preventDefault();
      beginLink();
    } else if (key === "x") {
      e.preventDefault();
      unlinkSelected();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      relCursor += 1;
      renderRelated();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      relCursor -= 1;
      renderRelated();
    } else if (e.key === "Enter") {
      e.preventDefault();
      const target = current?.related[relCursor];
      if (target) openEntry(target.path);
    }
    return;
  }

  // ------------------------------------------------------------ picker
  if (mode === "picker") {
    if (mod && e.key.toLowerCase() === "t") {
      e.preventDefault();
      cycleSort();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (linkFor) {
        const back = linkFor;
        linkFor = null;
        openEntry(back, false);
      } else {
        show("write");
      }
    } else if (e.key === "ArrowDown" || (mod && e.key.toLowerCase() === "j")) {
      e.preventDefault();
      movePicker(1);
    } else if (e.key === "ArrowUp" || (mod && e.key.toLowerCase() === "k")) {
      e.preventDefault();
      movePicker(-1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const target = shown[cursor];
      if (target) chooseInPicker(target);
    }
  }
});

// ---------------------------------------------------------------- wire-up

editor.addEventListener("input", () => {
  scheduleFit();
  scheduleDraftSave();
});

pickerFilter.addEventListener("input", () => {
  cursor = 0;
  renderPicker();
});

el<HTMLButtonElement>("setup-pick").addEventListener("click", chooseVault);

window.addEventListener("resize", scheduleFit);

// Keep focus on the page: clicking anywhere in write mode returns to the caret.
document.addEventListener("mousedown", (e) => {
  if (mode === "write" && e.target !== editor) {
    e.preventDefault();
    editor.focus();
  }
});

async function boot() {
  let vault: string | null = null;
  try {
    vault = await invoke<string | null>("get_vault");
  } catch {
    /* falls through to setup */
  }

  if (!vault) {
    show("setup");
    return;
  }

  try {
    const draft = await invoke<string>("load_draft");
    if (draft.trim()) {
      editor.value = draft;
      hud("draft restored");
    }
  } catch {
    /* no draft */
  }

  show("write");
  fit();
}

boot();
