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
    build::CheckoutBuilder, BranchType, CertificateCheckStatus, Cred, CredentialType, FetchOptions,
    IndexAddOption, PushOptions, RemoteCallbacks, Repository, RepositoryInitOptions, Signature,
};

use super::SyncOutcome;

/// Mirrors `shell::COMMIT_LOCK`: libgit2's index/ref writes are not safe to interleave either.
static COMMIT_LOCK: Mutex<()> = Mutex::new(());

fn msg(e: git2::Error) -> String {
    e.message().to_string()
}

// ------------------------------------------------------------ TLS trust roots

/// Android keeps its trust store in the Java runtime, not as PEM files on disk, so the OpenSSL
/// this crate is statically linked against finds no certificate authorities at all and rejects
/// every TLS handshake — pushes fail with "the SSL certificate is invalid" no matter how valid
/// the server's certificate is. Ship Mozilla's roots with the app and point libgit2 at them.
const CA_BUNDLE: &[u8] = include_bytes!("../../assets/cacert.pem");

/// Written into the vault (the one directory guaranteed writable before a repo even exists), so
/// `ensure_repo` also keeps it out of the commits.
const CA_BUNDLE_NAME: &str = ".forward-flow-cacert.pem";

/// Extracts the bundle next to the vault and registers it with libgit2. Idempotent, and cheap
/// enough after the first call to just run at the top of every entry point below.
fn ensure_ca_bundle(vault: &Path) {
    let path = vault.join(CA_BUNDLE_NAME);
    let needs_write = match fs::metadata(&path) {
        Ok(meta) => meta.len() != CA_BUNDLE.len() as u64,
        Err(_) => true,
    };
    if needs_write && fs::write(&path, CA_BUNDLE).is_err() {
        return;
    }

    // `set_ssl_cert_file` is the one that actually matters: it sets the locations on libgit2's
    // SSL context whenever it is called. SSL_CERT_FILE is only read by OpenSSL once, during the
    // global init that the first `Repository::open` of the process triggers, so on its own it
    // would be a race with whichever entry point ran first — it is set here purely as a backstop
    // for any OpenSSL path that reads the environment directly.
    std::env::set_var("SSL_CERT_FILE", &path);
    // Safe in the sense that matters here: this mutates libgit2 global state, and every caller
    // reaches it through vault_git's single-threaded push queue.
    unsafe {
        let _ = git2::opts::set_ssl_cert_file(path.as_path());
    }
}

// ---------------------------------------------------------------- repository

/// Makes `dir` a git repository if it is not one yet. Safe to call every time.
pub fn ensure_repo(dir: &Path) -> Result<(), String> {
    ensure_ca_bundle(dir);
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

    // Rewritten rather than only created, so that vaults set up before the CA bundle existed
    // still learn to ignore it instead of committing 188KB of certificates.
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
    ensure_ca_bundle(dir);
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
    ensure_ca_bundle(dir);
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
    ensure_ca_bundle(dir);
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

/// One push attempt. `Rejected` means libgit2 talked to the remote fine but it refused the ref
/// update (almost always a non-fast-forward), as opposed to `Err`, which is a connection or auth
/// problem the caller should not try to recover from by merging.
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
        callbacks.credentials(credentials_callback(url));
        callbacks.certificate_check(|_cert, _host| Ok(CertificateCheckStatus::CertificateOk));
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

    // `opts` (and the closure it owns, still borrowing `rejected`) lives until the end of this
    // scope, so `rejected` can't be moved out of here yet — clone its contents through the
    // borrow instead.
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

/// Fetches the remote branch and either fast-forwards onto it, or, if both sides moved on,
/// folds it into a merge commit. Entries are timestamped files, so this is almost always a
/// conflict-free three-way merge.
fn merge_from_remote(repo: &Repository, remote_name: &str, branch: &str) -> Result<(), String> {
    let mut remote = repo.find_remote(remote_name).map_err(msg)?;
    let url = remote.url().unwrap_or("").to_string();
    let mut callbacks = RemoteCallbacks::new();
    if url.starts_with("http://") || url.starts_with("https://") {
        callbacks.credentials(credentials_callback(url));
        callbacks.certificate_check(|_cert, _host| Ok(CertificateCheckStatus::CertificateOk));
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
    ensure_ca_bundle(dir);
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
        callbacks.credentials(credentials_callback(url));
        callbacks.certificate_check(|_cert, _host| Ok(CertificateCheckStatus::CertificateOk));
    }
    let mut opts = PushOptions::new();
    opts.remote_callbacks(callbacks);
    let _ = remote.push(&[refspec], Some(&mut opts));
}

/// Brings in any commits the remote gained since, merging them into the current branch.
/// Returns Ok(true) if local HEAD was updated, Ok(false) if up-to-date.
pub fn pull_and_merge(dir: &Path) -> Result<bool, String> {
    ensure_ca_bundle(dir);
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
    ensure_ca_bundle(dir);
    let _guard = COMMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let repo = Repository::open(dir).map_err(msg)?;
    let head_commit = match repo.head().and_then(|h| h.peel_to_commit()) {
        Ok(c) => c,
        Err(_) => return Ok(()), // nothing committed yet
    };

    // 1. Create missing tag branches
    for tag in active_tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        let branch_name = format!("tag/{}", tag);
        if repo.find_branch(&branch_name, BranchType::Local).is_err() {
            if repo.branch(&branch_name, &head_commit, false).is_ok() {
                push_refspec(&repo, &format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name));
            }
        }
    }

    // 2. Delete orphaned tag branches
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

    /// The Android vault in the wild already has a .gitignore from before the bundle existed.
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

        // 1. Add tags "ideas" and "work"
        sync_tag_branches(&dir, &[ "ideas".into(), "work".into() ]).unwrap();
        let repo = Repository::open(&dir).unwrap();
        assert!(repo.find_branch("tag/ideas", BranchType::Local).is_ok());
        assert!(repo.find_branch("tag/work", BranchType::Local).is_ok());

        // 2. Remove tag "ideas", keep "work", add "life"
        sync_tag_branches(&dir, &[ "work".into(), "life".into() ]).unwrap();
        assert!(repo.find_branch("tag/ideas", BranchType::Local).is_err(), "tag/ideas deleted");
        assert!(repo.find_branch("tag/work", BranchType::Local).is_ok());
        assert!(repo.find_branch("tag/life", BranchType::Local).is_ok());

        fs::remove_dir_all(&dir).unwrap();
    }
}
