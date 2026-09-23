//! Keeps the vault in git: every change is committed, then pushed to `origin` in the background.
//!
//! Git trouble never blocks or fails a save. By the time any of this runs the entry is already on
//! disk; a missing git, a bad network or a rejected push only changes the status the UI shows,
//! and the push is simply tried again on the next save or launch.
//!
//! Two backends implement the actual git work, chosen by platform: desktop shells out to the
//! system's own `git` (`shell.rs`), because one is normally already installed and that gets every
//! feature of it for free; Android has no such binary, so it talks git over an embedded libgit2
//! instead (`libgit2_backend.rs`, HTTPS-remotes-only — see that file for why). Everything below
//! this point — the sync queue, status events, `record()` — is the same either way.

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use tauri::{AppHandle, Emitter, Runtime};

mod shell;
// Compiled on every platform, but wired up only on Android below. Keeping it in the desktop build
// is what lets `cargo check` and `cargo test` see it at all: its own tests never ran anywhere
// while it was cfg'd out, and type errors in it surfaced only from an Android CI build.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod libgit2_backend;

#[cfg(not(target_os = "android"))]
use shell as backend;
#[cfg(target_os = "android")]
use libgit2_backend as backend;

struct PushState {
    running: bool,
    /// Folders with a save that landed while a push was in flight; pushed again once it finishes.
    pending: Vec<PathBuf>,
    /// Whether to report a successful sync; on launch only problems are worth showing.
    announce: bool,
}

static PUSH: Mutex<PushState> = Mutex::new(PushState {
    running: false,
    pending: Vec::new(),
    announce: false,
});

#[derive(Debug, PartialEq, Clone)]
pub enum SyncOutcome {
    /// The vault has no remote: entries are committed locally and nothing is pushed.
    NoRemote,
    Synced,
    /// The remote could not be reached. Nothing is lost; the next save retries.
    Offline(String),
    Failed(String),
}

#[derive(Clone, Serialize)]
struct StatusEvent {
    state: &'static str,
    detail: String,
}

fn emit<R: Runtime>(app: &AppHandle<R>, state: &'static str, detail: String) {
    let _ = app.emit("sync-status", StatusEvent { state, detail });
}

/// Pushes in a background thread. Requests that arrive while a push is running are merged into
/// one follow-up push, so a burst of saves does not queue a push each.
fn request_push<R: Runtime>(app: AppHandle<R>, dir: PathBuf, announce: bool) {
    {
        let mut state = PUSH.lock().unwrap_or_else(|e| e.into_inner());
        state.announce |= announce;
        if !state.pending.contains(&dir) {
            state.pending.push(dir);
        }
        if state.running {
            return; // the running push will pick this up when it finishes
        }
        state.running = true;
    }

    thread::spawn(move || loop {
        let (dirs, announce) = {
            let mut state = PUSH.lock().unwrap_or_else(|e| e.into_inner());
            (std::mem::take(&mut state.pending), state.announce)
        };

        for dir in dirs {
            match backend::push(&dir) {
                SyncOutcome::Synced if announce => emit(&app, "synced", String::new()),
                SyncOutcome::Offline(detail) => emit(&app, "offline", detail),
                SyncOutcome::Failed(detail) => emit(&app, "error", detail),
                _ => {}
            }
        }

        // Decide whether to stop under the same lock that new requests take, so a save arriving
        // right now is either seen here or starts a fresh push: never dropped.
        let mut state = PUSH.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending.is_empty() {
            state.running = false;
            state.announce = false;
            break;
        }
    });
}

/// Records a change: commits whatever is new in the vault, then pushes. Returns immediately; the
/// work happens in the background so writing is never slowed or blocked by git.
pub fn record<R: Runtime>(app: &AppHandle<R>, dir: PathBuf, message: String, announce: bool) {
    let app = app.clone();
    thread::spawn(move || {
        if let Err(e) = backend::ensure_repo(&dir).and_then(|_| backend::commit_all(&dir, &message)) {
            emit(&app, "error", e);
            return;
        }
        request_push(app, dir, announce);
    });
}

/// The vault's remote URL, if it has one — for the remote-setup screen to show what's configured
/// already (this is the whole URL, credentials included, so the caller decides how much of it is
/// safe to display).
pub fn get_remote(dir: &std::path::Path) -> Option<String> {
    backend::get_remote(dir)
}

/// Points the vault at `url` and immediately tries a push, so a bad URL or an unreachable host is
/// reported right away — through the same sync-status event a save's push would use — instead of
/// waiting silently until the next entry.
pub fn set_remote<R: Runtime>(app: &AppHandle<R>, dir: PathBuf, url: String) -> Result<(), String> {
    backend::set_remote(&dir, &url)?;
    request_push(app.clone(), dir, true);
    Ok(())
}

/// Pulls and merges remote changes into the vault, then pushes any local commits.
/// Emits "vault-updated" event if remote changes were pulled into the working tree.
pub fn sync_vault<R: Runtime>(app: &AppHandle<R>, dir: &std::path::Path, announce: bool) -> Result<bool, String> {
    backend::ensure_repo(dir)?;
    let updated = backend::pull_and_merge(dir).unwrap_or(false);
    let outcome = backend::push(dir);
    match outcome {
        SyncOutcome::Synced if announce => emit(app, "synced", String::new()),
        SyncOutcome::Offline(detail) => emit(app, "offline", detail),
        SyncOutcome::Failed(detail) => emit(app, "error", detail),
        _ => {}
    }
    if updated {
        let _ = app.emit("vault-updated", ());
    }
    Ok(updated)
}

/// Triggers an immediate sync in a background thread.
pub fn sync_now<R: Runtime>(app: &AppHandle<R>, dir: PathBuf, announce: bool) {
    let app = app.clone();
    thread::spawn(move || {
        let _ = sync_vault(&app, &dir, announce);
    });
}

/// Spawns a background timer to periodically auto-sync every 60 seconds if a remote exists.
pub fn start_background_sync<R: Runtime>(app: AppHandle<R>, dir: PathBuf) {
    thread::spawn(move || loop {
        thread::sleep(std::time::Duration::from_secs(60));
        if backend::get_remote(&dir).is_some() {
            let _ = sync_vault(&app, &dir, false);
        }
    });
}

/// Synchronizes git branches for active tags.
pub fn sync_tag_branches<R: Runtime>(app: &AppHandle<R>, dir: PathBuf, active_tags: Vec<String>) {
    let app = app.clone();
    thread::spawn(move || {
        if let Err(e) = backend::sync_tag_branches(&dir, &active_tags) {
            emit(&app, "error", format!("Branch sync failed: {}", e));
        }
    });
}

// ---------------------------------------------------------------- tests
//
// These exercise the background queue (record -> commit -> push -> status event) through
// whichever backend this platform dispatches to — `shell` on every host these tests actually run
// on. `shell.rs` and `libgit2_backend.rs` each additionally test their own commit/push logic
// directly.

#[cfg(test)]
mod tests {
    use super::shell::test_support::*;
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use tauri::Listener;

    /// record() pushes through process-wide state, so these tests take turns.
    static BACKGROUND: Mutex<()> = Mutex::new(());

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {}", what);
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn published_vault(name: &str) -> (PathBuf, PathBuf) {
        let remote = bare_remote(&format!("{}-remote", name));
        let dir = fresh(name);
        backend::ensure_repo(&dir).unwrap();
        add_remote(&dir, &remote);
        backend::commit_all(&dir, "Start Forward Flow vault").unwrap();
        assert_eq!(backend::push(&dir), SyncOutcome::Synced);
        (dir, remote)
    }

    #[test]
    fn a_save_is_committed_pushed_and_reported_without_the_caller_waiting() {
        let _turn = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, remote) = published_vault("record");
        let app = tauri::test::mock_app();
        let (tx, rx) = mpsc::channel();
        app.handle().listen("sync-status", move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        write(&dir, "2026-09-21-143012.md", "---\ncreated: x\n---\n\nhello\n");
        let started = Instant::now();
        record(app.handle(), dir.clone(), "Add entry 2026-09-21-143012".into(), true);
        assert!(started.elapsed() < Duration::from_millis(500), "record() must not block on git");

        let status = rx.recv_timeout(Duration::from_secs(20)).expect("no status event");
        assert!(status.contains("synced"), "unexpected status: {}", status);
        assert_eq!(log(&remote)[0], "Add entry 2026-09-21-143012");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn a_burst_of_saves_all_reach_the_remote() {
        let _turn = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, remote) = published_vault("burst");
        let app = tauri::test::mock_app();

        for i in 0..8 {
            write(&dir, &format!("2026-09-21-10000{}.md", i), &format!("entry {}\n", i));
            record(app.handle(), dir.clone(), format!("Add entry {}", i), true);
        }

        // Saves race each other, and a commit stages the whole vault, so one commit can carry a
        // neighbour's file and fewer than 8 commits is fine. What must hold: nothing is left
        // uncommitted, every entry reaches the remote, and the remote ends at the local head.
        wait_until("all 8 saves to be pushed", || {
            let local = git(&dir, &["rev-parse", "HEAD"]).unwrap_or_default();
            let pushed = git(&remote, &["rev-parse", "main"]).unwrap_or_default();
            !local.trim().is_empty() && local == pushed
                && git(&dir, &["status", "--porcelain"]).unwrap_or_default().trim().is_empty()
                && git(&remote, &["ls-tree", "--name-only", "main"])
                    .map(|tree| tree.lines().filter(|f| f.starts_with("2026-09-21-10000")).count() == 8)
                    .unwrap_or(false)
        });
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn a_failed_push_is_reported_and_the_next_save_retries_it() {
        let _turn = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, remote) = published_vault("retry");
        let app = tauri::test::mock_app();
        let (tx, rx) = mpsc::channel();
        app.handle().listen("sync-status", move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        // The remote disappears, as if the network were down.
        let parked = remote.with_extension("away");
        std::fs::rename(&remote, &parked).unwrap();
        write(&dir, "2026-09-21-100000.md", "written offline\n");
        record(app.handle(), dir.clone(), "Add entry offline".into(), true);
        let status = rx.recv_timeout(Duration::from_secs(20)).expect("no status event");
        assert!(status.contains("error") || status.contains("offline"), "got: {}", status);
        assert_eq!(log(&dir)[0], "Add entry offline", "the entry is committed locally regardless");

        // The remote is back; the next save pushes both the old and the new commit.
        std::fs::rename(&parked, &remote).unwrap();
        write(&dir, "2026-09-21-110000.md", "written online\n");
        record(app.handle(), dir.clone(), "Add entry online".into(), true);
        let status = rx.recv_timeout(Duration::from_secs(20)).expect("no status event");
        assert!(status.contains("synced"), "got: {}", status);
        assert_eq!(log(&remote)[..2], ["Add entry online", "Add entry offline"]);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&remote).unwrap();
    }
}
