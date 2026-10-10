# Forward Flow

Append-only writing, in two rooms: an editor that only moves forward, and a
viewer for sorting, tagging and linking what you already wrote.

A full-screen page with nothing on it. You write, you commit, the entry locks.
There is no way to go back and edit what you already committed — that is the
whole point.

## Running it

```sh
npm install
npm run tauri dev      # development
npm run tauri build    # produces an .app bundle
```

Rust must be on your PATH (`source "$HOME/.cargo/env"`, or add
`~/.cargo/bin` to your shell profile).

## Keys

| Key | Does |
| --- | --- |
| `⌘↵` or `⌘S` | Commit the entry — writes it to disk and locks it forever |
| `⌘O` | Open the viewer — browse committed entries |
| `⌘R` | Re-sync vault — fetch & merge latest changes from remote, push local commits |
| `⌘⇧O` | Change the folder your entries live in |
| `⌘⇧G` | Set the git remote to sync entries to |
| `⌘⌃F` | Toggle full screen |
| `↑` `↓` `⏎` | Move and open, inside the viewer |
| `⌘T` | Cycle sort: newest, oldest, longest, most linked |
| `esc` | Back to the page |

Inside an open entry:

| Key | Does |
| --- | --- |
| `t` | Edit tags — comma separated, `⏎` to save, `esc` to cancel |
| `l` | Link this entry to another — pick the other one from the list |
| `d` | Delete this entry — unlinks reciprocal links, removes file, and makes a revert commit |
| `↑` `↓` | Move through Related |
| `⏎` | Open the selected related entry |
| `x` | Unlink the selected related entry |

In the viewer's filter box, a query starting with `#` matches tags only;
anything else matches date, prose and tags together.

## Markdown

Write Markdown directly in the editor. Saved entries render headings, emphasis,
lists, blockquotes, links, images, tables, and fenced code blocks in the reader.
Links with full HTTP, HTTPS, or mailto URLs open in your default browser or mail
app. HTML is sanitized before display. The editor remains plain text and files
keep their Markdown source; rendering never rewrites your prose.

## Chat with a notebook

Choose a notebook (or Inbox) and use **Chat**. Set its **Notebook purpose** to
shape the assistant’s goals and style. Connect Gemini, Claude, or OpenAI under
**AI connections** using your API key and an available model ID. Named connections keep their own endpoint, API key, current model, and saved
model list. You can switch connections and models between turns.

When you send a message, the app sends the notebook purpose, conversation, and
pages directly in that notebook to the chosen provider. It includes up to 60 KB
of page text, newest first, skipping pages that do not fit. The response shows
how many pages were included. Child notebooks have separate context.

Replies render as Markdown. **Save as page** opens an editable review; **Save
page** then creates a new immutable Markdown entry in the same notebook. Your
writing draft is preserved. Chat history persists on this device; **New
conversation** starts another chat and keeps earlier conversations for `/resume`.

Purposes live in `.notebook.json` and sync with the vault. API keys and chat
histories live in app settings outside the vault, never in Git. Keys are stored
as local JSON with owner-only file permissions on Unix; they are not encrypted
in a system keychain. Provider API usage requires your own account and quota.
Replies arrive once generation finishes; this version does not stream tokens.

Chat shares the main writing screen with the editor: beside it on desktop and
below it on smaller screens. Selecting a notebook changes chat context while
preserving your writing draft. **Hide chat** expands the writing space; **Chat ↗**
reopens it. Purpose, AI connections, and MCP servers are under **Purpose & connections**.

### Notebook tools

All three providers can call tools during chat. Read, search, list, Git status,
and vault commit-history tools run immediately. Creating pages or child
notebooks, moving pages into child notebooks, tags, links, revisions, deletions,
and sync produce **proposals**. Review the arguments and original contents, then
choose **Apply change**. Unapplied proposals do not change files. Changes are
recorded and synced by the existing Git backend.

Page tools are restricted to the selected notebook. A revision creates a new
page, copies its tags, and links it to the preserved original. If a page changes
before a proposal is applied, the proposal is rejected. Each turn permits at
most six provider rounds with eight tool calls per round. Git history shows
vault commit summaries. Shell access and arbitrary Git operations require an
external MCP server exposing those tools. Markdown tables work; chart rendering
is not included yet.

### External MCP servers

Open **External MCP tools** in notebook chat. Enter a server name, its
Streamable HTTP endpoint (for example `https://example.com/mcp`), and an optional
bearer token, then choose **Connect & discover tools**. View discovered tools,
refresh the connection, disable its tools, or disconnect it from the same panel.
Enabled servers are available to notebook chat on this device.

The app supports Streamable HTTP with JSON or SSE responses and protocol
versions `2025-11-25`, `2025-06-18`, and `2025-03-26`. HTTPS is required except
for localhost endpoints. Legacy HTTP+SSE, stdio subprocesses, OAuth sign-in,
sampling, elicitation, resources, and prompts are not supported by this client.

Models can propose discovered tools from any enabled server. Every external
call requires **Approve & run external tool**, showing the server, endpoint,
tool, and arguments. The call’s text and structured result is stored in chat
history and included in subsequent messages to the selected LLM. Calls are not
automatically retried; an interrupted request might already have executed on
the server. An attempted call cannot be applied again from the same proposal.
Tool schemas are rechecked before invocation. With up to 64 external tools,
chat receives their definitions directly. Larger tool sets use
`mcp_discover_tools` to find tool schemas and `mcp_call_tool` to propose a call.
All enabled servers remain accessible, including servers added later in the
list. Discovery reads cached metadata without approval; remote execution still
requires approval. Each server supports up to 64 tools, with bounded response
and argument sizes. Settings and `/mcp` show when discovery is used.

Server settings and bearer tokens live in `config.json` in app settings,
with owner-only permissions on Unix. Tokens stay outside Git and are not sent
as tool definitions or LLM credentials. Tokens are stored locally without
keychain encryption. On Android, localhost refers to the phone itself.

On phones, **Write**, **Chat**, **Pages**, and **Settings** navigation keeps one
workspace visible at a time. Switching between writing and chat preserves your
draft and conversation. Desktop keeps the editor and chat side by side. Chat
**History** opens previous conversations; **Options** holds purpose and connection
settings.

Pending tool actions appear in a compact review panel. Choose **Review actions**,
inspect details, select the actions you want, and **Approve & run selected**.
Actions run in order and stop if one fails; completed external calls cannot be
run again from the same proposal. **Continue** sends a follow-up using the saved
results. Chat pauses as soon as an action needs approval, rather than spending
more provider rounds on the pending action. Read-only rounds retain their tool
budget; **Continue** starts another step when that budget is reached.

## Notebooks

Create a notebook with **+ New notebook** on the writing page or in the archive.
Choose a notebook before committing to save your next entry there. Existing
entries stay in **Inbox**, the vault root. The archive lets you browse **All pages**,
Inbox, or one notebook; open an entry and use **Move entry** to change its notebook.
Moving preserves the entry's prose, timestamp, tags, and links across notebooks.

Notebooks are ordinary subfolders of your vault, synced with the entries inside
them. Empty notebooks contain a `.gitkeep` file so they also sync. Existing nested
folders are listed as notebooks too. No migration is needed for existing vaults.

## Syncing

Every commit is pushed to a git remote in the background if one is set —
`⌘⇧G` (or the "remote" button on touch) opens a screen for pointing the
vault at one: `https://user:TOKEN@github.com/owner/repo.git`. A GitHub
[personal access token](https://github.com/settings/tokens) with the "repo"
scope works as the password. Without a remote, entries just stay local.

- **Background Auto-Sync**: The app automatically polls and merges remote changes every 60 seconds whenever a remote is configured.
- **Window Focus Sync**: Returning to or focusing the app immediately triggers a sync check so entries written on other devices surface right away.
- **Manual Re-sync**: Hit `⌘R` (or tap the "sync" button on touch, or "Sync now" in the remote screen) to immediately pull, merge, and push.
- **Live Updating**: When new commits arrive from another device, the viewer and open entries automatically refresh without needing an app restart.

On Android there is no folder picker, so the vault lives in the app's own
private storage automatically — setting a remote there is how entries leave
the device.

## How entries are stored

One markdown file per entry, in a folder you choose, named by timestamp:

```
2026-09-21-143012.md
```

```markdown
---
created: 2026-09-21T14:30:12+05:30
tags: [rivers, tooling]
links: [2026-09-20-101133]
---

whatever you wrote
```

Plain files in a plain folder. Move them, grep them, back them up, put them in
git. Nothing is hidden in a database.

Entry *bodies* are immutable; frontmatter is metadata and stays writable, so
tagging and linking never rewrite your prose. That rule is enforced by tests
(`cargo test`), not just by convention — including one that checks a metadata
write leaves the body byte-for-byte identical, and one that checks prose
containing a line like `tags: ...` is not mistaken for metadata.

Links are **symmetric**: linking A to B writes the link into both files, and
each entry shows a single *Related* list. If one half of the pair ever goes
missing, the other side still surfaces it, so a partial write self-heals.

Frontmatter keys this app does not recognise are preserved untouched, so you
can add your own fields by hand.

## Backup and sync

The entries folder is a git repository, and the app looks after it. Choosing a folder runs
`git init` for you (and commits what is already there). After that, every change is committed and
pushed in the background:

| You do | Commit |
| --- | --- |
| Commit an entry | `Add entry 2026-09-21-143012` |
| Delete an entry | `Revert entry 2026-09-21-143012` |
| Edit tags | `Tag 2026-09-21-143012` |
| Link or unlink two entries | `Link … and …` / `Unlink … and …` |
| Choose a folder, or launch the app | `Start Forward Flow vault` / `Sync vault` |

Saving never waits on git. The entry is written to disk first; committing and pushing happen on
another thread, so a slow network or a missing git cannot delay or lose a save. The hint line at
the bottom tells you how it went: `synced`, `offline · will sync on the next save`, or
`sync failed · <reason>`.

**Pushing needs a remote.** The app pushes to the folder's `origin`. Create an empty repository
somewhere (a private one, since these are your own writing) and connect it once:

```sh
cd ~/Documents/fflow
git remote add origin git@github.com:you/fflow.git
```

Without a remote the entries are still committed locally, and the app stays quiet about it.

### Tag-driven Git Branches

Adding tags to any note creates a dedicated git branch named `tag/<tag-name>` (e.g. `tag/ideas`,
`tag/work`) pointing to the latest commit and pushes it to the remote. When a tag is removed from an
entry (or when entries are deleted), if no note in the entire vault uses that tag anymore, the branch
`tag/<tag-name>` is automatically deleted both locally and on the remote.

Details worth knowing:

- Pushes that fail (offline, wrong key) are retried on the next save and on launch. Nothing is
  lost meanwhile: the commits are local and intact.
- If the remote has entries this machine lacks, they are rebased in before pushing. Entries are
  timestamped files, so this almost never conflicts.
- Rapid saves are merged into one push. A commit stages the whole folder, so anything edited
  outside the app is picked up too.
- Git runs without prompts: no passphrase, hook or signing dialogs. Use an SSH key without a
  passphrase (or one in your keychain) so pushes can run unattended.
- The app finds git at `/opt/homebrew/bin`, `/usr/local/bin` or `/usr/bin`. If none has it, saving
  works as before and the hint line says git is not available.
- Every version of every entry now lives in the repository's history, in addition to the
  append-only rule the editor enforces.

## What "append-only" means here

Immutability kicks in at commit, not at the keystroke. While drafting you can
backspace and rewrite freely. Once you hit `⌘↵`, that text is written to a new
file and the page clears. Reopening an old entry shows it in a reader — there
is no path back into the editor for text that already exists.

Drafts autosave to the app data directory, so quitting mid-thought doesn't
lose anything; the draft is restored on next launch and removed on commit.

## Typography

The text sizes itself: it starts large and shrinks as the entry grows, so the
page stays full without scrolling. Past a floor of 17px it stops shrinking and
scrolls, keeping the line you're writing in view.

## Backend layout

`src-tauri/src/lib.rs` wires up plugins, startup, and the Tauri command handler.
The backend is organized by responsibility:

- `commands/`: Tauri commands for vault settings, entries, and notebooks.
- `config.rs`: persisted vault configuration and paths.
- `drafts.rs`: draft storage and autosave commands.
- `entries.rs`: entry discovery, frontmatter, tags, and links.
- `notebooks.rs`: notebook folders, path validation, and entry moves.
- `state.rs`: the shared lock for vault writes and Android checkouts.
- `vault_git.rs` and `vault_git/`: background sync and platform Git backends.

Entry and notebook tests live alongside their modules in `entries/tests.rs` and
`notebooks/tests.rs`. Run backend checks with
`cargo test --manifest-path src-tauri/Cargo.toml`.

### Portable setup config

Use **Settings → Portable setup config → Export config** to move model, MCP,
and Git connections between devices. The welcome screen also has **Import setup
config**, so connections can be loaded before selecting a vault folder.
Import shows a summary and requires **Apply config**. It merges the supplied
connections by ID, keeps connections omitted from the file, and preserves the
current device's vault folder, drafts, and chat history. You can also import an
existing device's `config.json`; its vault path is ignored. Blank API keys keep
existing keys; blank MCP tokens keep a saved token only when the endpoint matches.

Settings now share one private `config.json` in the app config directory. Existing
AI and MCP settings migrate automatically; legacy files are removed after a
successful save. Chat history remains separate. The portable file has this format:

```json
{
  "version": 1,
  "git": { "remote": "https://github.com/owner/notebook.git" },
  "models": {
    "openai": { "provider": "openai", "model": "gpt-4.1-mini", "api_key": "" },
    "claude": { "provider": "claude", "model": "claude-sonnet-4-6", "api_key": "" },
    "gemini": { "provider": "gemini", "model": "gemini-3.8-flash", "api_key": "" }
  },
  "mcp_servers": {
    "research": {
      "id": "research",
      "url": "https://example.com/mcp",
      "token": "",
      "enabled": true
    }
  }
}
```

Export omits API keys, bearer tokens, and HTTPS Git credentials by default.
Select **Include API keys and tokens** for a complete transfer; the resulting
file contains readable credentials and must be kept private. Files are written
with owner-only permissions on Unix. No credentials go into the synced vault.

Import validates the entire file before applying it. It configures the Git remote
locally without starting a push or contacting MCP servers. Normal background
sync continues for an existing vault; choose **Sync now** to fetch entries.
Open notebook chat and use each new server's **Edit / refresh → Connect & discover
tools** before using it. Uploaded tool schemas are ignored; discovery comes from
the server. Supported MCP transports and authentication remain unchanged.

### Azure OpenAI

Select **Azure OpenAI** in **Purpose & connections → AI connections**. Enter the
resource endpoint (for example `https://your-resource.openai.azure.com`), your
Azure deployment name, and the resource API key. You can also use an endpoint
ending in `/openai/v1/`. The deployment must support the Responses API and tools.
The app uses `/openai/v1/responses` with API-key authentication; Microsoft Entra
sign-in and legacy versioned deployment endpoints are not supported.

Azure supports the same notebook tools and external MCP proposals as OpenAI.
Include it in a portable config under the `azure_openai` key:

```json
{
  "provider": "azure_openai",
  "model": "your-deployment-name",
  "endpoint": "https://your-resource.openai.azure.com",
  "api_key": ""
}
```

See [Microsoft's Azure Responses API guide](https://learn.microsoft.com/en-us/azure/ai-foundry/openai/how-to/responses?view=foundry-classic).

### Editor tools

Chat can use `read_editor` to read the unsaved draft captured when you send a
message. That draft is shared with your selected provider only when requested by
the tool. `replace_editor` proposes Markdown edits or formatting, `save_editor`
saves a page in the selected notebook without clearing the draft, and
`clear_editor` proposes clearing it. Changes require **Apply change** and are
rejected if the editor content, notebook, or revision has changed. Replacements
and clearing persist to the local draft; saved pages use the normal Git backup.

### Chat slash commands

Type `/` in the chat composer for suggestions. Use arrow keys to choose, Enter
or Tab to complete, and the send icon or ⌘/Ctrl+Enter to run. Commands execute
locally and are never sent to the LLM or added to conversation history.

| Command | Action |
| --- | --- |
| `/provider` or `/provider list` | List providers and select a connected one |
| `/provider azure_openai` | Switch the chat provider (also `openai`, `claude`, `gemini`) |
| `/model` or `/model list` | List models for the selected provider |
| `/model <model>` | Save a different model; Azure expects a deployment name |
| `/mcp` | Show saved MCP servers, enabled state, and discovered tool counts |
| `/mcp reconnect <server>` | Reconnect and refresh tools with the saved token |
| `/mcp reconnect all` | Reconnect all saved servers; disabled servers stay disabled |
| `/mcp enable <server>` / `/mcp disable <server>` | Enable or disable tools for chat |
| `/resume` | Choose an earlier conversation in the current notebook |
| `/resume <ID>` / `/resume latest` | Resume a specific chat or the most recently updated inactive chat |
| `/new` | Start a new conversation while retaining the previous one |
| `/help` | Show command help |

OpenAI, Claude, and Gemini model lists are fetched on demand using your saved
connection. Lists may contain models without text or tool support; choose a
compatible model. If discovery fails, `/model <model>` still accepts a known
model ID. Azure lists saved deployments; enter another deployment name from
your Azure resource with `/model <deployment>`.

Gemini may list older models that are unavailable to new users. A 404 from
`gemini-2.5-flash` can mean the model is unavailable for your key; switch with
`/provider gemini` followed by `/model gemini-3.8-flash`. The app sends Gemini
tool schemas through `parametersJsonSchema`, including notebook, editor, and
external MCP tools.

Conversation history stays on this device, outside the vault. Existing chat
files migrate when saved. Resuming keeps the current conversation and editor
draft and preserves tool approval/attempt status. It uses the currently selected
provider and model; the list shows the last provider/model used for each chat.
Chats deleted with the old **New conversation** behavior cannot be recovered.

### Saved models and named connections

A connection has a provider **type** (`openai`, `claude`, `gemini`, or
`azure_openai`) and a unique **name**. Multiple connections can share a type.
For example, `azure_openai` and `azure-voice` can have separate Azure resources
and API keys. Existing provider-named connections continue to work.

Under **AI connections**, choose **New connection**, enter `azure-voice` as the
connection name, select **Azure OpenAI**, and enter its endpoint, current
model/deployment, saved model list, and API key. Choose a saved connection in the
form to edit it. Blank keys retain the existing key only for the same connection,
provider type, and endpoint.

`/provider azure-voice` selects that connection. `/model saved` shows clickable
saved models without contacting the provider; `/model add <name>` saves a model
without switching. `/model <name>` switches and also keeps the model in the saved
list. Each connection supports up to 64 saved models; editing or importing adds
to the existing list.

Add named connections as keys inside the portable config's `models` object:

```json
{
  "azure-voice": {
    "provider": "azure_openai",
    "model": "your-current-deployment",
    "models": ["your-current-deployment", "another-deployment"],
    "endpoint": "https://your-voice-resource.openai.azure.com/openai/v1/",
    "api_key": ""
  }
}
```

The outer key is the connection name; `provider` selects its API format. `model`
is the active selection and `models` is the saved list. Older configs without
`models` use the active model as their initial saved model. A connection named
`azure-voice` uses the same text Responses API and notebook tools; its name does
not enable audio recording or speech output.
