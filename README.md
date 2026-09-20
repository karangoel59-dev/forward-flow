# Forward Flow

Append-only writing. Phase 1: the editor.

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
| `⌘O` | Browse committed entries (opens read-only) |
| `⌘⇧O` | Change the folder your entries live in |
| `⌘⌃F` | Toggle full screen |
| `↑` `↓` `⏎` | Move and open, inside the browser |
| `esc` | Back to the page |

## How entries are stored

One markdown file per entry, in a folder you choose, named by timestamp:

```
2026-09-21-143012.md
```

```markdown
---
created: 2026-09-21T14:30:12+05:30
tags: []
---

whatever you wrote
```

Plain files in a plain folder. Move them, grep them, back them up, put them in
git. Nothing is hidden in a database.

The `tags:` field is the seam for Phase 2 (the viewer — sorting, tagging,
linking). Entry *bodies* are immutable; frontmatter is metadata and stays
writable, so tagging later never rewrites your prose.

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
