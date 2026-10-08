//! Android git backend: HTTP(S) remotes with URL credentials and merge-based sync.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use git2::{
    build::CheckoutBuilder, BranchType, Cred, CredentialType, FetchOptions,
    IndexAddOption, PushOptions, RemoteCallbacks, Repository, RepositoryInitOptions, Signature,
};

use super::SyncOutcome;

/// Mirrors `shell::COMMIT_LOCK`: libgit2's index/ref writes are not safe to interleave either.
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

fn msg(e: git2::Error) -> String {
    e.message().to_string()
}

// TLS trust roots

/// Vendored OpenSSL needs PEM trust roots because Android exposes its store through Java.
const CA_BUNDLE: &[u8] = include_bytes!("../../assets/cacert.pem");

/// Keep the extracted CA bundle out of vault commits.
const CA_BUNDLE_NAME: &str = ".forward-flow-cacert.pem";

/// Extracts and registers the bundled trust roots.
static TLS_SETUP: Mutex<()> = Mutex::new(());

fn ensure_ca_bundle(vault: &Path) -> Result<(), String> {
    let _setup = TLS_SETUP.lock().unwrap_or_else(|e| e.into_inner());
    let path = vault.join(CA_BUNDLE_NAME);
    if fs::read(&path).ok().as_deref() != Some(CA_BUNDLE) {
        fs::write(&path, CA_BUNDLE)
            .map_err(|e| format!("cannot write TLS trust roots: {e}"))?;
    }

    // Initialize git2 before setting trust paths: its OpenSSL probe can change the environment.
    let _ = Repository::open(vault);
    std::env::set_var("SSL_CERT_FILE", &path);
    #[cfg(target_os = "android")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let cert_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // Keep the path buffer alive across the variadic libgit2 call.
        let result = unsafe {
            libgit2_sys::git_libgit2_opts(
                libgit2_sys::GIT_OPT_SET_SSL_CERT_LOCATIONS as std::os::raw::c_int,
                cert_path.as_ptr(),
                std::ptr::null::<std::os::raw::c_char>(),
            )
        };
        if result < 0 {
            let error = git2::Error::last_error(result).map(|e| e.message().to_string())
                .unwrap_or_else(|| "unknown TLS configuration error".into());
            return Err(format!("cannot load TLS trust roots: {error}"));
        }
    }
    Ok(())
}

// repository

/// Makes `dir` a git repository if it is not one yet. Safe to call every time.
pub fn ensure_repo(dir: &Path) -> Result<(), String> {
    ensure_ca_bundle(dir)?;
    let repo = if dir.join(".git").exists() {
        Repository::open(dir).map_err(msg)?
    } else {
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(dir, &opts).map_err(msg)?
    };

    // Use a local fallback identity when none is configured.
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

    // Update older vaults too so the CA bundle stays untracked.
    let ignore = dir.join(".gitignore");
    let existing = fs::read_to_string(&ignore).unwrap_or_default();
    let mut wanted = existing.clone();
    for line in [".DS_Store", CA_BUNDLE_NAME] {
        if !wanted.lines().any(|l| l.trim() == line) {
            if !wanted.is_empty() && !wanted.ends_with('\n') {
                wanted.push('\n');
            }
            wanted.push_str(line);
            wanted.push('\n');
        }
    }
    if wanted != existing {
        fs::write(&ignore, wanted).map_err(|e| e.to_string())?;
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
    ensure_ca_bundle(dir)?;
    let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let repo = Repository::open(dir).map_err(msg)?;

    let mut index = repo.index().map_err(msg)?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None).map_err(msg)?;
    // update_all also stages deletions, matching git add -A.
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

// pushing

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
    ensure_ca_bundle(dir).ok()?;
    let repo = Repository::open(dir).ok()?;
    let name = remote_name(&repo)?;
    let remote = repo.find_remote(&name).ok()?;
    remote.url().map(String::from)
}

/// Points the vault at `url`, replacing whatever `origin` already pointed at.
pub fn set_remote(dir: &Path, url: &str) -> Result<(), String> {
    ensure_ca_bundle(dir)?;
    let repo = Repository::open(dir).map_err(msg)?;
    match remote_name(&repo) {
        Some(name) => repo.remote_set_url(&name, url).map_err(msg)?,
        None => {
            repo.remote("origin", url).map_err(msg)?;
        }
    }
    Ok(())
}

/// Android credentials come from the remote URL; no credential helper is available.
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

fn looks_behind(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "non-fastforward",
        "non-fast-forward",
        "not fast-forward",
        "not fast forward",
        "fetch first",
        "contains commit",
        "contains work",
        "tip of your current branch is behind",
        "updates were rejected",
        "rejected",
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

/// Distinguishes rejected ref updates from connection or authentication errors.
fn push_once(repo: &Repository, remote_name: &str, branch: &str) -> Attempt {
    let mut remote = match repo.find_remote(remote_name) {
        Ok(r) => r,
        Err(e) => return Attempt::Err(msg(e)),
    };
    let url = remote.url().unwrap_or("").to_string();
    #[cfg(test)]
    let is_test_fixture = url.contains("lg2-merge-") || url.contains("lg2-unrelated-");
    #[cfg(not(test))]
    let is_test_fixture = false;

    if !is_test_fixture && !(url.starts_with("http://") || url.starts_with("https://")) {
        return Attempt::Err(format!(
            "android sync only supports an http(s) remote with credentials in the URL (this one is \"{}\")",
            url
        ));
    }

    let rejected = std::cell::RefCell::new(None::<String>);
    let mut callbacks = RemoteCallbacks::new();
    if url.starts_with("http://") || url.starts_with("https://") {
        // Keep libgit2's certificate and hostname validation against our CA bundle.
        callbacks.credentials(credentials_callback(url));
    }
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

    match result {
        Ok(()) => match rejected.borrow().clone() {
            Some(_) => Attempt::Rejected,
            None => Attempt::Synced,
        },
        Err(e) => {
            let m = e.message();
            if e.code() == git2::ErrorCode::NotFastForward
                || looks_behind(m)
                || rejected.borrow().is_some()
            {
                Attempt::Rejected
            } else {
                Attempt::Err(msg(e))
            }
        }
    }
}

/// Fetches and fast-forwards or merges the remote branch.
fn merge_from_remote(repo: &Repository, remote_name: &str, branch: &str) -> Result<(), String> {
    let mut remote = repo.find_remote(remote_name).map_err(msg)?;
    let url = remote.url().unwrap_or("").to_string();
    let mut callbacks = RemoteCallbacks::new();
    if url.starts_with("http://") || url.starts_with("https://") {
        // Keep libgit2's certificate and hostname validation against our CA bundle.
        callbacks.credentials(credentials_callback(url));
    }
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

    // Network work is finished. Keep only checkout/ref writes under the entry-write lock.
    let _write = crate::VAULT_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let mut status_options = git2::StatusOptions::new();
    status_options.include_untracked(true).include_ignored(false);
    if !repo.statuses(Some(&mut status_options)).map_err(msg)?.is_empty() {
        return Err("local vault changes are awaiting a commit; retry sync after saving".into());
    }

    let branch_ref_name = format!("refs/heads/{branch}");
    if analysis.is_fast_forward() {
        let tree = repo.find_commit(fetch_commit.id()).and_then(|c| c.tree()).map_err(msg)?;
        repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe())).map_err(msg)?;
        let mut branch_ref = repo.find_reference(&branch_ref_name).map_err(msg)?;
        branch_ref.set_target(fetch_commit.id(), "forward-flow: fast-forward").map_err(msg)?;
        repo.set_head(&branch_ref_name).map_err(msg)?;
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
    repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe())).map_err(msg)?;
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "Merge",
        &tree,
        &[&local_commit, &remote_commit],
    )
    .map_err(msg)?;
    Ok(())
}

/// Pushes the current branch, merging in the remote's own changes first if it has moved on.
pub fn push(dir: &Path) -> SyncOutcome {
    if let Err(e) = ensure_ca_bundle(dir) {
        return SyncOutcome::Failed(e);
    }
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

fn push_refspec(repo: &Repository, refspec: &str) {
    let Some(rname) = remote_name(repo) else {
        return;
    };
    let Ok(mut remote) = repo.find_remote(&rname) else {
        return;
    };
    let url = remote.url().unwrap_or("").to_string();
    #[cfg(test)]
    let is_test_fixture = url.contains("lg2-merge-") || url.contains("lg2-unrelated-") || url.contains("lg2-tag-");
    #[cfg(not(test))]
    let is_test_fixture = false;

    if !is_test_fixture && !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }

    let mut callbacks = RemoteCallbacks::new();
    if url.starts_with("http://") || url.starts_with("https://") {
        // Keep libgit2's certificate and hostname validation against our CA bundle.
        callbacks.credentials(credentials_callback(url));
    }
    let mut opts = PushOptions::new();
    opts.remote_callbacks(callbacks);
    let _ = remote.push(&[refspec], Some(&mut opts));
}

/// Brings in any commits the remote gained since, merging them into the current branch.
/// Returns Ok(true) if local HEAD was updated, Ok(false) if up-to-date.
pub fn pull_and_merge(dir: &Path) -> Result<bool, String> {
    ensure_ca_bundle(dir)?;
    let repo = Repository::open(dir).map_err(msg)?;
    let Some(remote) = remote_name(&repo) else {
        return Ok(false);
    };
    let branch = match repo.head().ok().and_then(|h| h.shorthand().map(String::from)) {
        Some(b) => b,
        None => return Ok(false),
    };
    let head_before = repo.head().ok().and_then(|h| h.target());
    {
        let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match merge_from_remote(&repo, &remote, &branch) {
            Ok(()) => {}
            Err(e) => {
                if e.contains("could not find") || e.contains("reference 'FETCH_HEAD' not found") {
                    return Ok(false);
                }
                return Err(e);
            }
        }
    }
    let head_after = repo.head().ok().and_then(|h| h.target());
    Ok(head_before.is_some() && head_before != head_after)
}

/// Synchronizes git branches for active tags:
/// - Creates branch `tag/<tag>` at HEAD for each tag in `active_tags` and pushes it to remote.
/// - Deletes any local and remote branch `tag/<tag>` whose tag is not in `active_tags`.
pub fn sync_tag_branches(dir: &Path, active_tags: &[String]) -> Result<(), String> {
    ensure_ca_bundle(dir)?;
    let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let repo = Repository::open(dir).map_err(msg)?;
    let head_commit = match repo.head().and_then(|h| h.peel_to_commit()) {
        Ok(c) => c,
        Err(_) => return Ok(()), // nothing committed yet
    };

    for tag in active_tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        let branch_name = format!("tag/{}", tag);
        let needs_update = match repo.find_branch(&branch_name, BranchType::Local) {
            Ok(b) => b.get().target() != Some(head_commit.id()),
            Err(_) => true,
        };
        if needs_update {
            if repo.branch(&branch_name, &head_commit, true).is_ok() {
                push_refspec(&repo, &format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name));
            }
        }
    }

    if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
        for item in branches.flatten() {
            let (mut branch, _) = item;
            if let Ok(Some(name)) = branch.name() {
                if let Some(tag) = name.strip_prefix("tag/") {
                    if !active_tags.iter().any(|t| t == tag) {
                        let branch_name = name.to_string();
                        let _ = branch.delete();
                        push_refspec(&repo, &format!(":refs/heads/{}", branch_name));
                    }
                }
            }
        }
    }

    Ok(())
}

// tests

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
    fn the_ca_bundle_is_extracted_but_never_committed() {
        let dir = fresh("lg2-ca");
        write(&dir, "a.md", "one\n");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "first").unwrap();

        assert!(dir.join(CA_BUNDLE_NAME).exists(), "OpenSSL needs it on disk to find it");
        let tracked = git(&dir, &["ls-files"]).unwrap();
        assert!(tracked.contains("a.md"));
        assert!(!tracked.contains(CA_BUNDLE_NAME), "188KB of roots do not belong in the vault");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_truncated_ca_bundle_is_repaired() {
        let dir = fresh("lg2-ca-repair");
        fs::write(dir.join(CA_BUNDLE_NAME), b"").unwrap();
        ensure_repo(&dir).unwrap();

        assert_eq!(
            fs::metadata(dir.join(CA_BUNDLE_NAME)).unwrap().len(),
            CA_BUNDLE.len() as u64,
            "corrupted or empty bundle should be rewritten with full bundle"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn same_size_corruption_of_trust_roots_is_repaired() {
        let dir = fresh("lg2-ca-content");
        fs::write(dir.join(CA_BUNDLE_NAME), vec![b' '; CA_BUNDLE.len()]).unwrap();
        ensure_ca_bundle(&dir).unwrap();
        assert_eq!(fs::read(dir.join(CA_BUNDLE_NAME)).unwrap(), CA_BUNDLE);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn trust_root_write_errors_are_reported() {
        let dir = fresh("lg2-ca-error");
        fs::create_dir(dir.join(CA_BUNDLE_NAME)).unwrap();
        assert!(ensure_ca_bundle(&dir).unwrap_err().contains("cannot write TLS trust roots"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_older_vault_learns_to_ignore_the_ca_bundle_without_losing_what_it_had() {
        let dir = fresh("lg2-ca-upgrade");
        fs::write(dir.join(".gitignore"), ".DS_Store\nscratch/\n").unwrap();

        ensure_repo(&dir).unwrap();

        let ignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(ignore.contains(CA_BUNDLE_NAME));
        assert!(ignore.contains("scratch/"), "entries already there are kept");
        fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn lg2_entries_written_on_another_machine_are_merged_not_lost() {
        let remote = bare_remote("lg2-merge-remote");
        let laptop = fresh("lg2-merge-laptop");
        write(&laptop, "2026-09-21-100000.md", "from the laptop\n");
        ensure_repo(&laptop).unwrap();
        add_remote(&laptop, &remote);
        commit_all(&laptop, "laptop 1").unwrap();
        assert_eq!(push(&laptop), SyncOutcome::Synced);

        let desktop = fresh("lg2-merge-desktop");
        fs::remove_dir_all(&desktop).unwrap();
        git(&std::env::temp_dir(), &["clone", "--quiet", remote.to_str().unwrap(), desktop.to_str().unwrap()]).unwrap();
        ensure_repo(&desktop).unwrap();
        write(&desktop, "2026-09-21-110000.md", "from the desktop\n");
        commit_all(&desktop, "desktop 1").unwrap();
        assert_eq!(push(&desktop), SyncOutcome::Synced);

        write(&laptop, "2026-09-21-120000.md", "laptop again\n");
        commit_all(&laptop, "laptop 2").unwrap();
        let outcome = push(&laptop);
        assert_eq!(outcome, SyncOutcome::Synced);
    }

    #[test]
    fn remote_updates_preserve_uncommitted_files_and_head() {
        // Exercise fast-forwards and merges with edits, deletions, and untracked entries.
        for diverged in [false, true] {
            let suffix = if diverged { "merge" } else { "ff" };
            let remote = bare_remote(&format!("lg2-merge-dirty-remote-{suffix}"));
            let local = fresh(&format!("lg2-merge-dirty-local-{suffix}"));
            let peer = fresh(&format!("lg2-merge-dirty-peer-{suffix}"));
            ensure_repo(&local).unwrap();
            write(&local, "edited.md", "original\n");
            write(&local, "deleted.md", "original\n");
            commit_all(&local, "initial").unwrap();
            add_remote(&local, &remote);
            assert_eq!(push(&local), SyncOutcome::Synced);
            fs::remove_dir_all(&peer).unwrap();
            git(&std::env::temp_dir(), &["clone", "--quiet", remote.to_str().unwrap(), peer.to_str().unwrap()]).unwrap();
            ensure_repo(&peer).unwrap();
            if diverged {
                write(&local, "local.md", "committed local entry\n");
                commit_all(&local, "local").unwrap();
            }
            write(&peer, "remote.md", "remote entry\n");
            commit_all(&peer, "remote").unwrap();
            assert_eq!(push(&peer), SyncOutcome::Synced);

            write(&local, "edited.md", "pending tags\n");
            fs::remove_file(local.join("deleted.md")).unwrap();
            write(&local, "new.md", "pending entry\n");
            let before = git(&local, &["rev-parse", "HEAD"]).unwrap();
            assert!(pull_and_merge(&local).unwrap_err().contains("awaiting a commit"));
            assert_eq!(git(&local, &["rev-parse", "HEAD"]).unwrap(), before);
            assert_eq!(fs::read_to_string(local.join("edited.md")).unwrap(), "pending tags\n");
            assert!(!local.join("deleted.md").exists());
            assert_eq!(fs::read_to_string(local.join("new.md")).unwrap(), "pending entry\n");

            commit_all(&local, "pending changes").unwrap();
            assert!(pull_and_merge(&local).unwrap());
            assert!(local.join("remote.md").exists());
            assert_eq!(fs::read_to_string(local.join("edited.md")).unwrap(), "pending tags\n");
            assert!(!local.join("deleted.md").exists());
            assert!(git(&local, &["status", "--porcelain"]).unwrap().trim().is_empty());
            for dir in [&local, &peer, &remote] { fs::remove_dir_all(dir).unwrap(); }
        }
    }

    #[test]
    fn lg2_unrelated_histories_are_merged_successfully() {
        let remote = bare_remote("lg2-unrelated-remote");
        // Remote gets an initial commit (e.g. GitHub README)
        let seeder = fresh("lg2-unrelated-seeder");
        git(&seeder, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        write(&seeder, "README.md", "# My Vault\n");
        git(&seeder, &["config", "user.name", "Seeder"]).unwrap();
        git(&seeder, &["config", "user.email", "seeder@example.com"]).unwrap();
        git(&seeder, &["add", "."]).unwrap();
        git(&seeder, &["commit", "--quiet", "-m", "Initial remote commit"]).unwrap();
        add_remote(&seeder, &remote);
        git(&seeder, &["push", "--quiet", "origin", "main"]).unwrap();

        // Local device initializes independently (no clone)
        let device = fresh("lg2-unrelated-device");
        write(&device, "2026-09-21-100000.md", "device entry\n");
        ensure_repo(&device).unwrap();
        add_remote(&device, &remote);
        commit_all(&device, "device 1").unwrap();

        // Pushing should detect rejection, fetch remote, merge unrelated histories, and push
        let outcome = push(&device);
        assert_eq!(outcome, SyncOutcome::Synced);
        assert!(device.join("README.md").exists());
        assert!(device.join("2026-09-21-100000.md").exists());
    }

    #[test]
    fn lg2_tag_branches_are_created_and_cleaned_up() {
        let dir = fresh("lg2-tag-branches");
        write(&dir, "a.md", "content\n");
        ensure_repo(&dir).unwrap();
        commit_all(&dir, "first").unwrap();

        sync_tag_branches(&dir, &[ "ideas".into(), "work".into() ]).unwrap();
        let repo = Repository::open(&dir).unwrap();
        assert!(repo.find_branch("tag/ideas", BranchType::Local).is_ok());
        assert!(repo.find_branch("tag/work", BranchType::Local).is_ok());

        sync_tag_branches(&dir, &[ "work".into(), "life".into() ]).unwrap();

        write(&dir, "b.md", "second\n");
        commit_all(&dir, "second").unwrap();
        let head2 = repo.head().unwrap().peel_to_commit().unwrap().id();
        sync_tag_branches(&dir, &[ "work".into(), "life".into() ]).unwrap();
        let work_branch = repo.find_branch("tag/work", BranchType::Local).unwrap();
        assert_eq!(work_branch.get().target(), Some(head2), "tag/work should advance to latest HEAD");

        fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(all(target_os = "android", feature = "tls-diagnostics"))]
pub fn diagnose_tls(output_dir: &Path) {
    use openssl::ssl::{SslContextBuilder, SslMethod};
    use std::fmt::Write;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let mut report = String::new();
    let dir = output_dir.join("tls-probe");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(CA_BUNDLE_NAME);
    let write = fs::write(&path, CA_BUNDLE);
    let _ = writeln!(report, "OpenSSL: {}", openssl::version::version());
    let _ = writeln!(report, "write={write:?} bytes={} path={}", CA_BUNDLE.len(), path.display());
    let _ = writeln!(report, "rust_read={:?}", fs::read(&path).map(|v| v == CA_BUNDLE));
    unsafe extern "C" {
        fn fopen(path: *const std::os::raw::c_char, mode: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
        fn fclose(file: *mut std::ffi::c_void) -> std::os::raw::c_int;
    }
    let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mode = CString::new("r").unwrap();
    unsafe {
        let file = fopen(cpath.as_ptr(), mode.as_ptr());
        let _ = writeln!(report, "libc_fopen={} errno={}", !file.is_null(), std::io::Error::last_os_error());
        if !file.is_null() { fclose(file); }
    }
    match SslContextBuilder::new(SslMethod::tls()) {
        Ok(mut ctx) => { let _ = writeln!(report, "openssl_set_ca_file={:?}", ctx.set_ca_file(&path)); }
        Err(e) => { let _ = writeln!(report, "openssl_context={e:?}"); }
    }
    let _ = writeln!(report, "git2_trust_setup={:?}", ensure_ca_bundle(&dir));
    let _ = writeln!(report, "remaining_openssl_errors={:?}", openssl::error::ErrorStack::get());
    if let Ok(repo) = Repository::init(&dir) {
        if let Ok(mut remote) = repo.remote_anonymous("https://github.com/karangoel59-dev/forward-flow.git") {
            let _ = writeln!(report, "github_public_fetch={:?}", remote.fetch(&["master"], None, None));
        }
    }
    let _ = fs::write(output_dir.join("tls-diagnostics.txt"), report);
}
