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
| `⌘⇧O` | Change the folder your entries live in |
| `⌘⌃F` | Toggle full screen |
| `↑` `↓` `⏎` | Move and open, inside the viewer |
| `⌘T` | Cycle sort: newest, oldest, longest, most linked |
| `esc` | Back to the page |

Inside an open entry:

| Key | Does |
| --- | --- |
| `t` | Edit tags — comma separated, `⏎` to save, `esc` to cancel |
| `l` | Link this entry to another — pick the other one from the list |
| `↑` `↓` | Move through Related |
| `⏎` | Open the selected related entry |
| `x` | Unlink the selected related entry |

In the viewer's filter box, a query starting with `#` matches tags only;
anything else matches date, prose and tags together.

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
