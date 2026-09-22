//! The Android backend: talks git over `libgit2` instead of shelling out.
//!
//! Android has no `git` binary and no shell PATH to find one on, so the desktop backend's
//! approach (`../shell.rs`) does not work there. This backend embeds libgit2 (via the `git2`
//! crate, vendored so nothing has to be present on the device) and reimplements the same three
//! operations: make the vault a repo, commit everything new, and push (merging in the remote's
//! own changes first when needed).
//!
//! Two differences from the desktop backend, both because of the constraints of running inside
//! a sandboxed mobile app rather than a full OS with a configured git:
//!
//! - **Only `http://`/`https://` remotes are supported**, with credentials embedded directly in
//!   the URL (`https://user:TOKEN@host/owner/repo.git`). There is no SSH agent, no `~/.ssh`, and
//!   no credential helper to fall back to on Android, so a personal access token in the URL is
//!   the one auth path that works without more UI than this app has today.
//! - **A real merge commit, not a rebase**, when the remote has commits we do not. libgit2 has no
//!   rebase-with-autostash equivalent to reach for the way the desktop backend uses `git pull
//!   --rebase --autostash`; a merge commit gives the same guarantee (nothing is lost, both
//!   machines' entries end up in history) at the cost of a less linear log.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use git2::{
    build::CheckoutBuilder, Cred, CredentialType, FetchOptions, IndexAddOption, PushOptions,
    RemoteCallbacks, Repository, RepositoryInitOptions, Signature,
};

use super::SyncOutcome;

/// Mirrors `shell::COMMIT_LOCK`: libgit2's index/ref writes are not safe to interleave either.
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

fn msg(e: git2::Error) -> String {
    e.message().to_string()
}

// ---------------------------------------------------------------- repository

/// Makes `dir` a git repository if it is not one yet. Safe to call every time.
pub fn ensure_repo(dir: &Path) -> Result<(), String> {
    let repo = if dir.join(".git").exists() {
        Repository::open(dir).map_err(msg)?
    } else {
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(dir, &opts).map_err(msg)?
    };

    // Commits need an author. Use whatever identity is configured (there usually is none at all
    // on a fresh Android install); only if there is none fall back to a local one.
    let has_identity = repo
        .config()
        .and_then(|c| c.get_string("user.email"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_identity {
        let mut local = repo.config().map_err(msg)?.open_level(git2::ConfigLevel::Local).map_err(msg)?;
        local.set_str("user.email", "forward-flow@localhost").map_err(msg)?;
        local.set_str("user.name", "Forward Flow").map_err(msg)?;
    }

    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, ".DS_Store\n").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn signature(repo: &Repository) -> Result<Signature<'static>, String> {
    repo.signature()
        .or_else(|_| Signature::now("Forward Flow", "forward-flow@localhost"))
        .map_err(msg)
}

/// Stages everything in the vault and commits it. Returns false when there was nothing new.
pub fn commit_all(dir: &Path, message: &str) -> Result<bool, String> {
    let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let repo = Repository::open(dir).map_err(msg)?;

    let mut index = repo.index().map_err(msg)?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None).map_err(msg)?;
    // `add_all` alone mirrors `git add <path>` (new + modified); files removed from the working
    // tree also need `update_all` to be staged as deletions, matching `git add -A`.
    index.update_all(["*"], None).map_err(msg)?;
    index.write().map_err(msg)?;
    let tree = repo.find_tree(index.write_tree().map_err(msg)?).map_err(msg)?;

    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    if head_tree.as_ref().map(|t| t.id()) == Some(tree.id()) {
        return Ok(false);
    }

    let sig = signature(&repo)?;
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents).map_err(msg)?;
    Ok(true)
}

// ---------------------------------------------------------------- pushing

fn remote_name(repo: &Repository) -> Option<String> {
    let names = repo.remotes().ok()?;
    let names: Vec<&str> = names.iter().flatten().collect();
    if names.contains(&"origin") {
        Some("origin".to_string())
    } else {
        names.first().map(|s| s.to_string())
    }
}

/// The vault's remote URL, if it has one, for showing back in the remote-setup screen.
pub fn get_remote(dir: &Path) -> Option<String> {
    let repo = Repository::open(dir).ok()?;
    let name = remote_name(&repo)?;
    // `find_remote(...).ok()?` as part of the tail expression ties the temporary `Remote`'s drop
    // to `repo`'s in a way the borrow checker won't accept (same shape as the push_once fix
    // earlier this session) — bind it first so it's dropped, in order, before `repo` is.
    let remote = repo.find_remote(&name).ok()?;
    remote.url().map(String::from)
}

/// Points the vault at `url`, replacing whatever `origin` already pointed at.
pub fn set_remote(dir: &Path, url: &str) -> Result<(), String> {
    let repo = Repository::open(dir).map_err(msg)?;
    match remote_name(&repo) {
        Some(name) => repo.remote_set_url(&name, url).map_err(msg)?,
        None => {
            repo.remote("origin", url).map_err(msg)?;
        }
    }
    Ok(())
}

/// Pulls `user:pass` (or `user:token`) straight out of an `https://user:pass@host/...` URL. This
/// is the only credential source on Android: no SSH agent, no keychain, no credential helper.
fn url_credentials(url: &str) -> Option<(String, String)> {
    let after_scheme = url.split("://").nth(1)?;
    let userinfo = after_scheme.split('/').next()?.split('@').next()?;
    if !after_scheme.contains('@') {
        return None;
    }
    let (user, pass) = userinfo.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn credentials_callback(url: String) -> impl FnMut(&str, Option<&str>, CredentialType) -> Result<Cred, git2::Error> {
    move |_url, _username_from_url, _allowed| {
        url_credentials(&url)
            .map(|(user, pass)| Cred::userpass_plaintext(&user, &pass))
            .unwrap_or_else(|| {
                Err(git2::Error::from_str(
                    "android sync needs the remote URL to carry a token, e.g. https://user:TOKEN@host/owner/repo.git",
                ))
            })
    }
}

fn looks_offline(m: &str) -> bool {
    let m = m.to_lowercase();
    if m.contains("401") || m.contains("403") || m.contains("authentication") || m.contains("not found") || m.contains("credentials") {
        return false;
    }
    [
        "failed to resolve address",
        "could not resolve host",
        "network is unreachable",
        "no route to host",
        "connection timed out",
        "operation timed out",
        "connection refused",
        "connection reset",
        "failed to connect",
        "could not connect",
        "timed out",
    ]
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

enum Attempt {
    Synced,
    Rejected,
    Err(String),
}

/// One push attempt. `Rejected` means libgit2 talked to the remote fine but it refused the ref
/// update (almost always a non-fast-forward), as opposed to `Err`, which is a connection or auth
/// problem the caller should not try to recover from by merging.
fn push_once(repo: &Repository, remote_name: &str, branch: &str) -> Attempt {
    let mut remote = match repo.find_remote(remote_name) {
        Ok(r) => r,
        Err(e) => return Attempt::Err(msg(e)),
    };
    let url = remote.url().unwrap_or("").to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Attempt::Err(format!(
            "android sync only supports an http(s) remote with credentials in the URL (this one is \"{}\")",
            url
        ));
    }

    let rejected = std::cell::RefCell::new(None::<String>);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(credentials_callback(url));
    callbacks.push_update_reference(|_refname, status| {
        if let Some(reason) = status {
            *rejected.borrow_mut() = Some(reason.to_string());
        }
        Ok(())
    });

    let mut opts = PushOptions::new();
    opts.remote_callbacks(callbacks);
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let result = remote.push(&[refspec.as_str()], Some(&mut opts));

    // `opts` (and the closure it owns, still borrowing `rejected`) lives until the end of this
    // scope, so `rejected` can't be moved out of here yet — clone its contents through the
    // borrow instead.
    match result {
        Ok(()) => match rejected.borrow().clone() {
            Some(_) => Attempt::Rejected,
            None => Attempt::Synced,
        },
        Err(e) => Attempt::Err(msg(e)),
    }
}

/// Fetches the remote branch and either fast-forwards onto it, or, if both sides moved on,
/// folds it into a merge commit. Entries are timestamped files, so this is almost always a
/// conflict-free three-way merge.
fn merge_from_remote(repo: &Repository, remote_name: &str, branch: &str) -> Result<(), String> {
    let mut remote = repo.find_remote(remote_name).map_err(msg)?;
    let url = remote.url().unwrap_or("").to_string();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(credentials_callback(url));
    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    remote.fetch(&[branch], Some(&mut fetch_opts), None).map_err(msg)?;
    drop(remote);

    let fetch_head = repo.find_reference("FETCH_HEAD").map_err(msg)?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head).map_err(msg)?;
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit]).map_err(msg)?;

    if analysis.is_up_to_date() {
        return Ok(());
    }

    let branch_ref_name = format!("refs/heads/{branch}");
    if analysis.is_fast_forward() {
        let mut branch_ref = repo.find_reference(&branch_ref_name).map_err(msg)?;
        branch_ref.set_target(fetch_commit.id(), "forward-flow: fast-forward").map_err(msg)?;
        repo.set_head(&branch_ref_name).map_err(msg)?;
        repo.checkout_head(Some(CheckoutBuilder::new().force())).map_err(msg)?;
        return Ok(());
    }

    let local_commit = repo.head().and_then(|h| h.peel_to_commit()).map_err(msg)?;
    let remote_commit = repo.find_commit(fetch_commit.id()).map_err(msg)?;

    let mut merged_index = repo.merge_commits(&local_commit, &remote_commit, None).map_err(msg)?;
    if merged_index.has_conflicts() {
        return Err("the vault was edited on another device in a way that conflicts here".to_string());
    }
    let tree = repo.find_tree(merged_index.write_tree_to(repo).map_err(msg)?).map_err(msg)?;

    let sig = signature(repo)?;
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "Merge",
        &tree,
        &[&local_commit, &remote_commit],
    )
    .map_err(msg)?;
    repo.checkout_head(Some(CheckoutBuilder::new().force())).map_err(msg)?;
    Ok(())
}

/// Pushes the current branch, merging in the remote's own changes first if it has moved on.
pub fn push(dir: &Path) -> SyncOutcome {
    let repo = match Repository::open(dir) {
        Ok(r) => r,
        Err(e) => return classify(msg(e)),
    };
    let Some(remote) = remote_name(&repo) else {
        return SyncOutcome::NoRemote;
    };
    let branch = match repo.head().ok().and_then(|h| h.shorthand().map(String::from)) {
        Some(b) => b,
        None => return SyncOutcome::Failed("nothing committed yet".to_string()),
    };

    match push_once(&repo, &remote, &branch) {
        Attempt::Synced => return SyncOutcome::Synced,
        Attempt::Rejected => {}
        Attempt::Err(e) => return classify(e),
    }

    {
        let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = merge_from_remote(&repo, &remote, &branch) {
            return classify(e);
        }
    }

    match push_once(&repo, &remote, &branch) {
        Attempt::Synced => SyncOutcome::Synced,
        Attempt::Rejected => SyncOutcome::Failed("push rejected again after merging the remote's changes".to_string()),
        Attempt::Err(e) => classify(e),
    }
}

// ---------------------------------------------------------------- tests
//
// These exercise the libgit2 backend directly (bypassing the platform dispatcher in
// `vault_git.rs`, which only routes here `#[cfg(target_os = "android")]`), against bare remotes
// set up with the real `git` CLI via `shell::test_support`. They compile only for Android, so
// running them needs an Android target test runner (there is none in this workspace); the
// Android CI build (`tauri android build`) is what actually compiles this module today.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault_git::shell::test_support::*;
    use std::fs;

    #[test]
    fn a_new_folder_becomes_a_repo_with_its_existing_entries_committed() {
        let dir = fresh("lg2-init");
        write(&dir, "2026-09-21-010807.md", "---\ncreated: x\n---\n\nhello\n");

        ensure_repo(&dir).unwrap();
        assert!(commit_all(&dir, "Start Forward Flow vault").unwrap());

        assert!(dir.join(".git").exists());
        assert_eq!(log(&dir), vec!["Start Forward Flow vault"]);
        let tracked = git(&dir, &["ls-files"]).unwrap();
        assert!(tracked.contains("2026-09-21-010807.md"));
        assert!(tracked.contains(".gitignore"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn setup_is_safe_to_repeat_and_does_not_recommit_unchanged_files() {
        let dir = fresh("lg2-repeat");
        write(&dir, "a.md", "one\n");

        ensure_repo(&dir).unwrap();
        assert!(commit_all(&dir, "first").unwrap());
        ensure_repo(&dir).unwrap();
        assert!(!commit_all(&dir, "second").unwrap(), "nothing changed, so no commit");

        assert_eq!(log(&dir), vec!["first"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_vault_without_a_remote_just_commits_locally() {
        let dir = fresh("lg2-noremote");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "first").unwrap();

        assert_eq!(push(&dir), SyncOutcome::NoRemote);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The bare remote created by `shell::test_support::bare_remote` has no credentials, but a
    /// filesystem-path "remote" is not http(s) at all, so the push is rejected before libgit2
    /// even needs any — this is the same guardrail real android:// setups rely on.
    #[test]
    fn a_non_https_remote_is_rejected_up_front() {
        let dir = fresh("lg2-scheme");
        let remote = bare_remote("lg2-scheme-remote");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        add_remote(&dir, &remote);
        commit_all(&dir, "first").unwrap();

        assert!(matches!(push(&dir), SyncOutcome::Failed(e) if e.contains("http")));
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&remote).unwrap();
    }

    #[test]
    fn url_credentials_are_pulled_out_of_the_remote_url() {
        assert_eq!(
            url_credentials("https://octocat:ghp_abc123@github.com/octocat/vault.git"),
            Some(("octocat".to_string(), "ghp_abc123".to_string()))
        );
        assert_eq!(url_credentials("https://github.com/octocat/vault.git"), None);
    }

    #[test]
    fn a_dropped_connection_is_told_apart_from_a_setup_problem() {
        assert!(looks_offline("failed to resolve address for github.com: nodename nor servname provided"));
        assert!(looks_offline("Failed to connect to github.com port 443: Connection timed out"));
        assert!(!looks_offline("unexpected http status code: 401"));
        assert!(!looks_offline("remote error: Repository not found."));
    }
}
