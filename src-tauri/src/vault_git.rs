//! Keeps the vault in git: every change is committed, then pushed to `origin` in the background.
//!
//! Git trouble never blocks or fails a save. By the time any of this runs the entry is already on
//! disk; a missing git, a bad network or a rejected push only changes the status the UI shows,
//! and the push is simply tried again on the next save or launch.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Runtime};

/// Nothing git does here should take this long; a hung network must not wedge the queue.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// One commit (or rebase) at a time: git's own index lock would otherwise make concurrent
/// saves fail with "another git process seems to be running".
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

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

// ---------------------------------------------------------------- running git

/// Desktop apps do not inherit the shell's PATH, so look in the usual places first.
fn git_binary() -> PathBuf {
    ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("git"))
}

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

fn kill(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill").arg(pid.to_string()).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status();
}

/// Runs git in `dir`, never prompting for credentials and giving up after `GIT_TIMEOUT`.
fn run(dir: &Path, args: &[&str]) -> Result<Output, String> {
    let child = Command::new(git_binary())
        .arg("-C")
        .arg(dir)
        // Automatic commits must never stop to ask for a passphrase or run someone's hooks.
        .args(["-c", "commit.gpgsign=false", "-c", "core.quotepath=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git is not available ({})", e))?;

    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(GIT_TIMEOUT) {
        Ok(Ok(out)) => Ok(Output {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            kill(pid);
            Err("git timed out".to_string())
        }
    }
}

/// Like `run`, but a non-zero exit is an error carrying git's own message.
fn ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = run(dir, args)?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(last_line(&out.stderr, &out.stdout))
    }
}

fn last_line(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    text.trim().lines().last().unwrap_or("git failed").trim().to_string()
}

// ---------------------------------------------------------------- repository

/// Makes `dir` a git repository if it is not one yet, so a freshly chosen folder is versioned
/// from its first entry. Safe to call every time.
pub fn ensure_repo(dir: &Path) -> Result<(), String> {
    if !dir.join(".git").exists() {
        ok(dir, &["init", "--quiet", "--initial-branch=main"])?;
    }

    // Commits need an author. Use the person's own git identity; only if there is none anywhere
    // fall back to a local one so saving still works.
    let has_identity = ok(dir, &["config", "user.email"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_identity {
        ok(dir, &["config", "user.email", "forward-flow@localhost"])?;
        ok(dir, &["config", "user.name", "Forward Flow"])?;
    }

    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, ".DS_Store\n").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Stages everything in the vault and commits it. Returns false when there was nothing new.
pub fn commit_all(dir: &Path, message: &str) -> Result<bool, String> {
    let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    ok(dir, &["add", "-A"])?;
    // `--quiet` makes the exit code the answer: 0 means nothing is staged.
    if run(dir, &["diff", "--cached", "--quiet"])?.success {
        return Ok(false);
    }
    ok(dir, &["commit", "--quiet", "--no-verify", "-m", message])?;
    Ok(true)
}

// ---------------------------------------------------------------- pushing

fn remote_name(dir: &Path) -> Option<String> {
    let remotes = ok(dir, &["remote"]).ok()?;
    let names: Vec<&str> = remotes.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if names.contains(&"origin") {
        Some("origin".to_string())
    } else {
        names.first().map(|s| s.to_string())
    }
}

fn looks_offline(msg: &str) -> bool {
    let m = msg.to_lowercase();
    // A refused key or a missing repository also ends in "could not read from remote", but that
    // is a setup problem to report, not a connection to wait out.
    if m.contains("permission denied") || m.contains("authentication failed") || m.contains("not found") {
        return false;
    }
    [
        "could not resolve host",
        "network is unreachable",
        "no route to host",
        "connection timed out",
        "operation timed out",
        "connection refused",
        "connection reset",
        "failed to connect",
        "timed out",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// The remote has commits we do not, e.g. entries written on another machine.
fn looks_behind(msg: &str) -> bool {
    let m = msg.to_lowercase();
    ["non-fast-forward", "fetch first", "tip of your current branch is behind"]
        .iter()
        .any(|needle| m.contains(needle))
}

fn classify(err: String) -> SyncOutcome {
    if looks_offline(&err) {
        SyncOutcome::Offline(err)
    } else {
        SyncOutcome::Failed(err)
    }
}

/// Pushes the current branch, first bringing in anything the remote gained since. Entries are
/// timestamped files, so merging another machine's work is almost always conflict-free.
pub fn push(dir: &Path) -> SyncOutcome {
    let Some(remote) = remote_name(dir) else {
        return SyncOutcome::NoRemote;
    };
    let push_args = ["push", "--quiet", "--set-upstream", remote.as_str(), "HEAD"];

    let first = match run(dir, &push_args) {
        Ok(out) => out,
        Err(e) => return classify(e),
    };
    if first.success {
        return SyncOutcome::Synced;
    }
    let err = last_line(&first.stderr, &first.stdout);
    if !looks_behind(&first.stderr) {
        return classify(err);
    }

    let branch = match ok(dir, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(b) => b.trim().to_string(),
        Err(e) => return SyncOutcome::Failed(e),
    };
    {
        let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pulled = run(
            dir,
            &["pull", "--rebase", "--autostash", "--quiet", remote.as_str(), branch.as_str()],
        );
        match pulled {
            Ok(out) if out.success => {}
            Ok(out) => {
                let _ = run(dir, &["rebase", "--abort"]);
                return classify(last_line(&out.stderr, &out.stdout));
            }
            Err(e) => return classify(e),
        }
    }

    match run(dir, &push_args) {
        Ok(out) if out.success => SyncOutcome::Synced,
        Ok(out) => classify(last_line(&out.stderr, &out.stdout)),
        Err(e) => classify(e),
    }
}

// ---------------------------------------------------------------- background work

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
            match push(&dir) {
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
        if let Err(e) = ensure_repo(&dir).and_then(|_| commit_all(&dir, &message)) {
            emit(&app, "error", e);
            return;
        }
        request_push(app, dir, announce);
    });
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty directory under the system temp dir.
    fn fresh(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-git-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    fn log(dir: &Path) -> Vec<String> {
        ok(dir, &["log", "--format=%s"])
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    fn bare_remote(name: &str) -> PathBuf {
        let remote = fresh(name);
        ok(&remote, &["init", "--quiet", "--bare", "--initial-branch=main"]).unwrap();
        remote
    }

    fn add_remote(dir: &Path, remote: &Path) {
        ok(dir, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
    }

    #[test]
    fn a_new_folder_becomes_a_repo_with_its_existing_entries_committed() {
        let dir = fresh("init");
        write(&dir, "2026-09-21-010807.md", "---\ncreated: x\n---\n\nhello\n");

        ensure_repo(&dir).unwrap();
        assert!(commit_all(&dir, "Start Forward Flow vault").unwrap());

        assert!(dir.join(".git").exists());
        assert_eq!(log(&dir), vec!["Start Forward Flow vault"]);
        let tracked = ok(&dir, &["ls-files"]).unwrap();
        assert!(tracked.contains("2026-09-21-010807.md"));
        assert!(tracked.contains(".gitignore"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn setup_is_safe_to_repeat_and_does_not_recommit_unchanged_files() {
        let dir = fresh("repeat");
        write(&dir, "a.md", "one\n");

        ensure_repo(&dir).unwrap();
        assert!(commit_all(&dir, "first").unwrap());
        ensure_repo(&dir).unwrap();
        assert!(!commit_all(&dir, "second").unwrap(), "nothing changed, so no commit");

        assert_eq!(log(&dir), vec!["first"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_kind_of_change_becomes_its_own_commit() {
        let dir = fresh("changes");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "Start Forward Flow vault").unwrap();

        write(&dir, "2026-09-21-143012.md", "---\ncreated: x\ntags: []\n---\n\nbody\n");
        commit_all(&dir, "Add entry 2026-09-21-143012").unwrap();
        // A tag edit rewrites the frontmatter only.
        write(&dir, "2026-09-21-143012.md", "---\ncreated: x\ntags: [rivers]\n---\n\nbody\n");
        commit_all(&dir, "Tag 2026-09-21-143012").unwrap();

        assert_eq!(
            log(&dir),
            vec![
                "Tag 2026-09-21-143012",
                "Add entry 2026-09-21-143012",
                "Start Forward Flow vault"
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_vault_without_a_remote_just_commits_locally() {
        let dir = fresh("noremote");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "first").unwrap();

        assert_eq!(push(&dir), SyncOutcome::NoRemote);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_push_publishes_the_branch_and_later_pushes_add_to_it() {
        let dir = fresh("push");
        let remote = bare_remote("push-remote");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        add_remote(&dir, &remote);
        commit_all(&dir, "first").unwrap();
        assert_eq!(push(&dir), SyncOutcome::Synced);

        write(&dir, "b.md", "two\n");
        commit_all(&dir, "second").unwrap();
        assert_eq!(push(&dir), SyncOutcome::Synced);
        assert_eq!(push(&dir), SyncOutcome::Synced, "nothing new is still a success");

        assert_eq!(log(&remote), vec!["second", "first"]);
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn entries_written_on_another_machine_are_merged_not_lost() {
        let remote = bare_remote("merge-remote");
        let laptop = fresh("merge-laptop");
        write(&laptop, "2026-09-21-100000.md", "from the laptop\n");
        ensure_repo(&laptop).unwrap();
        add_remote(&laptop, &remote);
        commit_all(&laptop, "laptop 1").unwrap();
        assert_eq!(push(&laptop), SyncOutcome::Synced);

        // A second machine clones, writes an entry and pushes it first.
        let desktop = fresh("merge-desktop");
        fs::remove_dir_all(&desktop).unwrap();
        ok(&std::env::temp_dir(), &["clone", "--quiet", remote.to_str().unwrap(), desktop.to_str().unwrap()])
            .unwrap();
        ensure_repo(&desktop).unwrap();
        write(&desktop, "2026-09-21-110000.md", "from the desktop\n");
        commit_all(&desktop, "desktop 1").unwrap();
        assert_eq!(push(&desktop), SyncOutcome::Synced);

        // The laptop writes again without having seen that: its push is rejected, then rebased.
        write(&laptop, "2026-09-21-120000.md", "laptop again\n");
        commit_all(&laptop, "laptop 2").unwrap();
        assert_eq!(push(&laptop), SyncOutcome::Synced);

        for name in ["2026-09-21-100000.md", "2026-09-21-110000.md", "2026-09-21-120000.md"] {
            assert!(laptop.join(name).exists(), "{} missing locally", name);
        }
        let mut pushed = log(&remote);
        pushed.sort();
        assert_eq!(
            pushed,
            vec!["desktop 1", "laptop 1", "laptop 2"],
            "every machine's commits reached the remote"
        );
        for dir in [&remote, &laptop, &desktop] {
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_broken_remote_never_loses_the_committed_entry() {
        let dir = fresh("broken");
        ok(&dir, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        ok(&dir, &["remote", "add", "origin", "/no/such/place.git"]).unwrap();
        write(&dir, "a.md", "precious\n");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "Add entry a").unwrap();

        assert!(matches!(push(&dir), SyncOutcome::Failed(_) | SyncOutcome::Offline(_)));
        assert_eq!(log(&dir), vec!["Add entry a"], "the commit stays local and intact");
        assert_eq!(fs::read_to_string(dir.join("a.md")).unwrap(), "precious\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_still_works_when_git_has_no_identity_configured() {
        let dir = fresh("identity");
        write(&dir, "a.md", "one\n");
        // Hide the real git config so this behaves like a machine that has never used git.
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");

        ensure_repo(&dir).unwrap();
        let committed = commit_all(&dir, "first");
        let author = ok(&dir, &["log", "-1", "--format=%an <%ae>"]);

        std::env::remove_var("GIT_CONFIG_GLOBAL");
        std::env::remove_var("GIT_CONFIG_NOSYSTEM");
        assert!(committed.unwrap());
        assert_eq!(author.unwrap().trim(), "Forward Flow <forward-flow@localhost>");
        fs::remove_dir_all(&dir).unwrap();
    }

    // ---- the background path the real app uses: record() -> commit -> push -> status event

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
        ensure_repo(&dir).unwrap();
        add_remote(&dir, &remote);
        commit_all(&dir, "Start Forward Flow vault").unwrap();
        assert_eq!(push(&dir), SyncOutcome::Synced);
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
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&remote).unwrap();
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
            let local = ok(&dir, &["rev-parse", "HEAD"]).unwrap_or_default();
            let pushed = ok(&remote, &["rev-parse", "main"]).unwrap_or_default();
            !local.trim().is_empty() && local == pushed
                && ok(&dir, &["status", "--porcelain"]).unwrap_or_default().trim().is_empty()
                && ok(&remote, &["ls-tree", "--name-only", "main"])
                    .map(|tree| tree.lines().filter(|f| f.starts_with("2026-09-21-10000")).count() == 8)
                    .unwrap_or(false)
        });
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&remote).unwrap();
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
        fs::rename(&remote, &parked).unwrap();
        write(&dir, "2026-09-21-100000.md", "written offline\n");
        record(app.handle(), dir.clone(), "Add entry offline".into(), true);
        let status = rx.recv_timeout(Duration::from_secs(20)).expect("no status event");
        assert!(status.contains("error") || status.contains("offline"), "got: {}", status);
        assert_eq!(log(&dir)[0], "Add entry offline", "the entry is committed locally regardless");

        // The remote is back; the next save pushes both the old and the new commit.
        fs::rename(&parked, &remote).unwrap();
        write(&dir, "2026-09-21-110000.md", "written online\n");
        record(app.handle(), dir.clone(), "Add entry online".into(), true);
        let status = rx.recv_timeout(Duration::from_secs(20)).expect("no status event");
        assert!(status.contains("synced"), "got: {}", status);
        assert_eq!(log(&remote)[..2], ["Add entry online", "Add entry offline"]);
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn a_dropped_connection_is_told_apart_from_a_setup_problem() {
        assert!(looks_offline("fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com"));
        assert!(looks_offline("ssh: connect to host github.com port 22: Operation timed out"));
        assert!(looks_offline("git timed out"));
        // Same "could not read from remote" ending, but this one will not fix itself.
        assert!(!looks_offline("git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository."));
        assert!(!looks_offline("ERROR: Repository not found."));
    }

    #[test]
    fn only_a_behind_remote_triggers_a_merge() {
        assert!(looks_behind("! [rejected] main -> main (fetch first)"));
        assert!(looks_behind("! [rejected] main -> main (non-fast-forward)"));
        assert!(!looks_behind("! [remote rejected] main -> main (protected branch hook declined)"));
    }
}
