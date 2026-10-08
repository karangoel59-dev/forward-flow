//! Background vault commits and sync: system git on desktop, libgit2 on Android.

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use tauri::{AppHandle, Emitter, Runtime};

mod shell;
// Compile on desktop too so Android code and tests are checked locally.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod libgit2_backend;

#[cfg(not(target_os = "android"))]
use shell as backend;
#[cfg(target_os = "android")]
use libgit2_backend as backend;

// Keep a tag-branch update after its corresponding commit, including rapid saves.
static RECORD: Mutex<()> = Mutex::new(());

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

        // Check under the request lock so a concurrent save cannot be dropped.
        let mut state = PUSH.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending.is_empty() {
            state.running = false;
            state.announce = false;
            break;
        }
    });
}

/// Commits and pushes in the background; the caller has already saved to disk.
pub fn record<R: Runtime>(app: &AppHandle<R>, dir: PathBuf, message: String, announce: bool) {
    let app = app.clone();
    thread::spawn(move || {
        let _record = RECORD.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = backend::ensure_repo(&dir).and_then(|_| backend::commit_all(&dir, &message)) {
            emit(&app, "error", e);
            return;
        }
        let active = crate::entries::collect_active_tags(&dir);
        if let Err(e) = backend::sync_tag_branches(&dir, &active) {
            emit(&app, "error", format!("Branch sync failed: {}", e));
        }
        request_push(app, dir, announce);
    });
}

/// Returns the remote URL, including credentials; mask it before displaying.
pub fn get_remote(dir: &std::path::Path) -> Option<String> {
    backend::get_remote(dir)
}

/// Sets the remote and queues a push, reporting failures through sync-status.
pub fn set_remote<R: Runtime>(app: &AppHandle<R>, dir: PathBuf, url: String) -> Result<(), String> {
    backend::set_remote(&dir, &url)?;
    request_push(app.clone(), dir, true);
    Ok(())
}

/// Pulls and merges remote changes into the vault, then pushes any local commits.
/// Emits "vault-updated" event if remote changes were pulled into the working tree.
pub fn sync_vault<R: Runtime>(app: &AppHandle<R>, dir: &std::path::Path, announce: bool) -> Result<bool, String> {
    backend::ensure_repo(dir)?;
    let updated = backend::pull_and_merge(dir).map_err(|detail| {
        emit(app, "error", detail.clone());
        detail
    })?;
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
pub fn start_background_sync(app: AppHandle) {
    thread::spawn(move || loop {
        thread::sleep(std::time::Duration::from_secs(60));
        if let Ok(dir) = crate::config::vault_dir(&app) {
            if backend::get_remote(&dir).is_some() {
                let _ = sync_vault(&app, &dir, false);
            }
        }
    });
}

// tests

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

        // Commits may include several saves; every entry must reach the remote.
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
    fn recording_tags_publishes_branches_at_the_new_commit() {
        let _turn = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, remote) = published_vault("record-tags");
        let app = tauri::test::mock_app();
        let (tx, rx) = mpsc::channel();
        app.handle().listen("sync-status", move |event| {
            let _ = tx.send(event.payload().to_string());
        });
        write(&dir, "a.md", "---\ncreated: x\ntags: [ideas]\nlinks: []\n---\n\nentry\n");
        record(app.handle(), dir.clone(), "Tag entry".into(), true);
        let status = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(status.contains("synced"), "{status}");
        let head = git(&dir, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(git(&dir, &["rev-parse", "tag/ideas"]).unwrap(), head);
        assert_eq!(git(&remote, &["rev-parse", "tag/ideas"]).unwrap(), head);
        wait_until("push queue idle", || !PUSH.lock().unwrap().running);
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

#[cfg(all(target_os = "android", feature = "tls-diagnostics"))]
pub fn diagnose_tls(dir: PathBuf) {
    thread::spawn(move || libgit2_backend::diagnose_tls(&dir));
}
