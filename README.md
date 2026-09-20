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
