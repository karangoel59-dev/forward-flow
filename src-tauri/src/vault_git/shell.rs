//! The desktop backend: shells out to the system's own `git` binary.
//!
//! This is the backend used everywhere except Android (see `../libgit2_backend.rs`), because on
//! desktop a real `git` is normally installed already, and running it directly gets every feature
//! (any remote scheme, credential helpers, the user's own config) for free.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::thread;

use super::SyncOutcome;

/// Nothing git does here should take this long; a hung network must not wedge the queue.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// One commit (or rebase) at a time: git's own index lock would otherwise make concurrent
/// saves fail with "another git process seems to be running".
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

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

/// The vault's remote URL, if it has one, for showing back in the remote-setup screen.
pub fn get_remote(dir: &Path) -> Option<String> {
    let name = remote_name(dir)?;
    ok(dir, &["remote", "get-url", &name]).ok().map(|s| s.trim().to_string())
}

/// Points the vault at `url`, replacing whatever `origin` already pointed at.
pub fn set_remote(dir: &Path, url: &str) -> Result<(), String> {
    match remote_name(dir) {
        Some(name) => ok(dir, &["remote", "set-url", &name, url])?,
        None => ok(dir, &["remote", "add", "origin", url])?,
    };
    Ok(())
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

// ---------------------------------------------------------------- shared test helpers
//
// Exposed to sibling backend test modules too, so the libgit2 backend's tests can set up and
// inspect fixtures (bare remotes, commit logs) with a real `git` CLI regardless of which backend
// is under test.

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A fresh empty directory under the system temp dir.
    pub(crate) fn fresh(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-git-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    pub(crate) fn log(dir: &Path) -> Vec<String> {
        ok(dir, &["log", "--format=%s"])
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    pub(crate) fn bare_remote(name: &str) -> PathBuf {
        let remote = fresh(name);
        ok(&remote, &["init", "--quiet", "--bare", "--initial-branch=main"]).unwrap();
        remote
    }

    pub(crate) fn add_remote(dir: &Path, remote: &Path) {
        ok(dir, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
    }

    pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        ok(dir, args)
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

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
    fn setting_a_remote_on_a_vault_that_had_none_lets_it_push() {
        let dir = fresh("set-remote-fresh");
        let remote = bare_remote("set-remote-fresh-remote");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "first").unwrap();
        assert_eq!(push(&dir), SyncOutcome::NoRemote);

        assert_eq!(get_remote(&dir), None);
        set_remote(&dir, remote.to_str().unwrap()).unwrap();
        assert_eq!(get_remote(&dir).as_deref(), Some(remote.to_str().unwrap()));
        assert_eq!(push(&dir), SyncOutcome::Synced);

        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn setting_a_remote_again_replaces_it_rather_than_erroring() {
        let dir = fresh("set-remote-replace");
        let first_remote = bare_remote("set-remote-replace-first");
        let second_remote = bare_remote("set-remote-replace-second");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        add_remote(&dir, &first_remote);
        commit_all(&dir, "first").unwrap();

        set_remote(&dir, second_remote.to_str().unwrap()).unwrap();
        assert_eq!(get_remote(&dir).as_deref(), Some(second_remote.to_str().unwrap()));
        assert_eq!(push(&dir), SyncOutcome::Synced);
        assert_eq!(log(&second_remote), vec!["first"]);
        // An empty bare repo has no HEAD to log at all — `git log` errors rather than returning
        // nothing, which is itself the confirmation that first_remote never received a push.
        assert!(
            git(&first_remote, &["log", "--format=%s"]).is_err(),
            "the old remote never saw the push"
        );

        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&first_remote).unwrap();
        fs::remove_dir_all(&second_remote).unwrap();
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
