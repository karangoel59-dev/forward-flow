//! Lists and resolves files that conflict between this device and the last fetched remote commit.

use std::path::{Path, PathBuf};

use git2::{build::CheckoutBuilder, Commit, Index, IndexEntry, IndexTime, Repository};
use serde::{Deserialize, Serialize};

use crate::entries::{dedupe, meta_from, rewrite_meta_text, split_frontmatter};

/// Longer versions are cut for display only; resolving always uses the full contents.
const MAX_SHOWN: usize = 100_000;

#[derive(Serialize, Debug)]
pub struct ConflictFile {
    pub path: String,
    /// `None` when this side deleted the file.
    pub mine: Option<String>,
    pub theirs: Option<String>,
    /// How "keep both" works here: "combine" merges tags and links, "copy" saves a second file.
    pub both: Option<&'static str>,
}

#[derive(Serialize, Debug, Default)]
pub struct Conflicts {
    pub local: String,
    pub remote: String,
    pub files: Vec<ConflictFile>,
}

#[derive(Deserialize)]
pub struct Choice {
    pub path: String,
    pub keep: String,
}

struct Raw {
    path: String,
    /// Every path the conflict occupies in the index; they differ when one side renamed the file.
    paths: Vec<Vec<u8>>,
    mine: Option<Vec<u8>>,
    theirs: Option<Vec<u8>>,
}

fn msg(e: git2::Error) -> String {
    e.message().to_string()
}

/// HEAD and the fetched remote commit, or `None` when there is nothing to compare yet.
fn sides(repo: &Repository) -> Option<(Commit<'_>, Commit<'_>)> {
    let local = repo.head().ok()?.peel_to_commit().ok()?;
    let remote = repo
        .find_reference("FETCH_HEAD")
        .ok()?
        .peel_to_commit()
        .ok()?;
    Some((local, remote))
}

fn merged(repo: &Repository, local: &Commit, remote: &Commit) -> Result<(Index, Vec<Raw>), String> {
    let index = repo.merge_commits(local, remote, None).map_err(msg)?;
    let mut raws = Vec::new();
    {
        let blob = |entry: &Option<IndexEntry>| -> Result<Option<Vec<u8>>, String> {
            entry
                .as_ref()
                .map(|e| repo.find_blob(e.id).map(|b| b.content().to_vec()).map_err(msg))
                .transpose()
        };
        for conflict in index.conflicts().map_err(msg)? {
            let conflict = conflict.map_err(msg)?;
            let mut paths: Vec<Vec<u8>> = Vec::new();
            for entry in [&conflict.our, &conflict.their, &conflict.ancestor]
                .into_iter()
                .flatten()
            {
                if !paths.contains(&entry.path) {
                    paths.push(entry.path.clone());
                }
            }
            let Some(first) = paths.first() else {
                continue;
            };
            raws.push(Raw {
                path: String::from_utf8_lossy(first).into_owned(),
                mine: blob(&conflict.our)?,
                theirs: blob(&conflict.their)?,
                paths,
            });
        }
    }
    Ok((index, raws))
}

fn both_mode(raw: &Raw) -> Option<&'static str> {
    let (mine, theirs) = (raw.mine.as_ref()?, raw.theirs.as_ref()?);
    if raw.path.ends_with(".md") {
        if let (Ok(mine), Ok(theirs)) = (std::str::from_utf8(mine), std::str::from_utf8(theirs)) {
            // Entry prose is immutable, so matching bodies mean only tags or links diverged.
            if split_frontmatter(mine).1 == split_frontmatter(theirs).1 {
                return Some("combine");
            }
        }
    }
    Some("copy")
}

fn combine(mine: &str, theirs: &str) -> String {
    let path = PathBuf::from("entry.md");
    let (a, b) = (meta_from(&path, mine), meta_from(&path, theirs));
    rewrite_meta_text(
        mine,
        Some(dedupe(a.tags.into_iter().chain(b.tags).collect())),
        Some(dedupe(a.links.into_iter().chain(b.links).collect())),
    )
}

fn shown(bytes: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return "(This version is not text and cannot be shown.)".into();
    };
    if text.len() <= MAX_SHOWN {
        return text.into();
    }
    let mut end = MAX_SHOWN;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (cut for display)", &text[..end])
}

/// A free path beside `path` for the other device's version. Entry names stay unique vault-wide.
fn copy_path(index: &Index, path: &str) -> String {
    let original = Path::new(path);
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let extension = original
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let taken: Vec<String> = index
        .iter()
        .filter_map(|e| {
            Path::new(&String::from_utf8_lossy(&e.path).into_owned())
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem}-other-device")
        } else {
            format!("{stem}-other-device-{n}")
        };
        if !taken.contains(&name) {
            let file = format!("{name}{extension}");
            return match original.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(parent) => format!("{}/{file}", parent.to_string_lossy().replace('\\', "/")),
                None => file,
            };
        }
        n += 1;
    }
}

fn put(repo: &Repository, index: &mut Index, path: &str, data: &[u8]) -> Result<(), String> {
    let id = repo.blob(data).map_err(msg)?;
    index
        .add(&IndexEntry {
            ctime: IndexTime::new(0, 0),
            mtime: IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: data.len() as u32,
            id,
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        })
        .map_err(msg)
}

/// Conflicts between HEAD and the last fetch. Reads only local data; fetch first for fresh results.
pub fn list(dir: &Path) -> Result<Conflicts, String> {
    let repo = Repository::open(dir).map_err(msg)?;
    let Some((local, remote)) = sides(&repo) else {
        return Ok(Conflicts::default());
    };
    let (_, raws) = merged(&repo, &local, &remote)?;
    Ok(Conflicts {
        local: local.id().to_string(),
        remote: remote.id().to_string(),
        files: raws
            .iter()
            .map(|raw| ConflictFile {
                path: raw.path.clone(),
                mine: raw.mine.as_deref().map(shown),
                theirs: raw.theirs.as_deref().map(shown),
                both: both_mode(raw),
            })
            .collect(),
    })
}

/// Merges the fetched remote commit into HEAD using one choice per conflicting file.
/// `local` and `remote` are the commits the user reviewed; anything newer is rejected.
pub fn resolve(dir: &Path, local: &str, remote: &str, choices: &[Choice]) -> Result<(), String> {
    let repo = Repository::open(dir).map_err(msg)?;
    let (ours, theirs) = sides(&repo).ok_or("There is no sync conflict to resolve")?;
    if ours.id().to_string() != local || theirs.id().to_string() != remote {
        return Err("The vault changed since you reviewed this conflict. Review it again.".into());
    }
    let mut status = git2::StatusOptions::new();
    status.include_untracked(true).include_ignored(false);
    if !repo.statuses(Some(&mut status)).map_err(msg)?.is_empty() {
        return Err("Local vault changes are awaiting a commit. Try again in a moment.".into());
    }

    let (mut index, raws) = merged(&repo, &ours, &theirs)?;
    if raws.is_empty() {
        return Err("There is no sync conflict to resolve".into());
    }
    for raw in &raws {
        let keep = choices
            .iter()
            .find(|c| c.path == raw.path)
            .map(|c| c.keep.as_str())
            .ok_or("Choose a version for every file")?;
        // Drop the ancestor, ours and theirs stages; a side that lacks the file has no stage.
        for path in &raw.paths {
            let path = String::from_utf8_lossy(path).into_owned();
            for stage in 1..=3 {
                let _ = index.remove(Path::new(&path), stage);
            }
        }
        match (keep, both_mode(raw)) {
            ("mine", _) => {
                if let Some(data) = &raw.mine {
                    put(&repo, &mut index, &raw.path, data)?;
                }
            }
            ("theirs", _) => {
                if let Some(data) = &raw.theirs {
                    put(&repo, &mut index, &raw.path, data)?;
                }
            }
            ("both", Some("combine")) => {
                let mine = String::from_utf8_lossy(raw.mine.as_deref().unwrap_or_default());
                let other = String::from_utf8_lossy(raw.theirs.as_deref().unwrap_or_default());
                put(&repo, &mut index, &raw.path, combine(&mine, &other).as_bytes())?;
            }
            ("both", Some(_)) => {
                put(&repo, &mut index, &raw.path, raw.mine.as_deref().unwrap_or_default())?;
                let copy = copy_path(&index, &raw.path);
                put(&repo, &mut index, &copy, raw.theirs.as_deref().unwrap_or_default())?;
            }
            _ => return Err("That choice is not available for this file".into()),
        }
    }
    if index.has_conflicts() {
        return Err("Some conflicts could not be resolved".into());
    }

    let tree = repo
        .find_tree(index.write_tree_to(&repo).map_err(msg)?)
        .map_err(msg)?;
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("Forward Flow", "forward-flow@localhost"))
        .map_err(msg)?;
    repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))
        .map_err(msg)?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Resolve sync conflict",
        &tree,
        &[&ours, &theirs],
    )
    .map_err(msg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::shell::{self, test_support::*};
    use super::*;
    use std::fs;

    const BODY: &str = "Shared prose that never changes.\n";

    fn entry(tags: &str, links: &str) -> String {
        format!("---\ncreated: x\ntags: [{tags}]\nlinks: [{links}]\n---\n\n{BODY}")
    }

    /// Two devices that both changed `a.md` and `notes.txt`; `mine` has fetched but not merged.
    fn diverged(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let remote = bare_remote(&format!("conflict-{name}-remote"));
        let mine = fresh(&format!("conflict-{name}-mine"));
        write(&mine, "a.md", &entry("", ""));
        write(&mine, "notes.txt", "original\n");
        write(&mine, "untouched.md", "left alone\n");
        shell::ensure_repo(&mine).unwrap();
        add_remote(&mine, &remote);
        shell::commit_all(&mine, "start").unwrap();
        assert_eq!(shell::push(&mine), crate::vault_git::SyncOutcome::Synced);

        let other = fresh(&format!("conflict-{name}-other"));
        fs::remove_dir_all(&other).unwrap();
        git(
            &std::env::temp_dir(),
            &["clone", "--quiet", remote.to_str().unwrap(), other.to_str().unwrap()],
        )
        .unwrap();
        shell::ensure_repo(&other).unwrap();
        write(&other, "a.md", &entry("theirs, shared", "2026-01-01-000000"));
        write(&other, "notes.txt", "from the other device\n");
        write(&other, "remote-only.md", "new elsewhere\n");
        shell::commit_all(&other, "other device").unwrap();
        assert_eq!(shell::push(&other), crate::vault_git::SyncOutcome::Synced);

        write(&mine, "a.md", &entry("mine, shared", ""));
        write(&mine, "notes.txt", "from this device\n");
        shell::commit_all(&mine, "this device").unwrap();
        assert!(shell::fetch(&mine).unwrap());
        (mine, other, remote)
    }

    fn choose(conflicts: &Conflicts, keep: &str) -> Vec<Choice> {
        conflicts
            .files
            .iter()
            .map(|f| Choice {
                path: f.path.clone(),
                keep: keep.into(),
            })
            .collect()
    }

    fn cleanup(dirs: [&PathBuf; 3]) {
        for dir in dirs {
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn both_versions_are_listed_without_touching_the_vault() {
        let (mine, other, remote) = diverged("list");
        let head = git(&mine, &["rev-parse", "HEAD"]).unwrap();
        let conflicts = list(&mine).unwrap();

        let paths: Vec<_> = conflicts.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "notes.txt"]);
        assert!(conflicts.files[0].mine.as_ref().unwrap().contains("mine, shared"));
        assert!(conflicts.files[0].theirs.as_ref().unwrap().contains("theirs, shared"));
        assert_eq!(conflicts.files[0].both, Some("combine"));
        assert_eq!(conflicts.files[1].both, Some("copy"));
        assert_eq!(conflicts.local, head.trim());

        assert_eq!(git(&mine, &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(git(&mine, &["status", "--porcelain"]).unwrap().trim().is_empty());
        assert!(!mine.join("remote-only.md").exists());
        cleanup([&mine, &other, &remote]);
    }

    #[test]
    fn a_vault_with_nothing_fetched_or_nothing_diverged_has_no_conflicts() {
        let dir = fresh("conflict-none");
        write(&dir, "a.md", "one\n");
        shell::ensure_repo(&dir).unwrap();
        shell::commit_all(&dir, "first").unwrap();
        assert!(list(&dir).unwrap().files.is_empty());
        assert!(resolve(&dir, "", "", &[]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeping_one_side_merges_everything_else_and_pushes_cleanly() {
        for keep in ["mine", "theirs"] {
            let (mine, other, remote) = diverged(&format!("keep-{keep}"));
            let conflicts = list(&mine).unwrap();
            resolve(&mine, &conflicts.local, &conflicts.remote, &choose(&conflicts, keep)).unwrap();

            let expected = if keep == "mine" { "from this device\n" } else { "from the other device\n" };
            assert_eq!(fs::read_to_string(mine.join("notes.txt")).unwrap(), expected);
            assert!(fs::read_to_string(mine.join("a.md")).unwrap().contains(&format!("{keep}, shared")));
            assert_eq!(fs::read_to_string(mine.join("remote-only.md")).unwrap(), "new elsewhere\n");
            assert_eq!(fs::read_to_string(mine.join("untouched.md")).unwrap(), "left alone\n");
            assert!(git(&mine, &["status", "--porcelain"]).unwrap().trim().is_empty());
            assert!(list(&mine).unwrap().files.is_empty());
            assert_eq!(shell::push(&mine), crate::vault_git::SyncOutcome::Synced);
            assert_eq!(log(&remote)[0], "Resolve sync conflict");
            cleanup([&mine, &other, &remote]);
        }
    }

    #[test]
    fn keeping_both_combines_entry_metadata_and_copies_other_files() {
        let (mine, other, remote) = diverged("both");
        let conflicts = list(&mine).unwrap();
        resolve(&mine, &conflicts.local, &conflicts.remote, &choose(&conflicts, "both")).unwrap();

        let raw = fs::read_to_string(mine.join("a.md")).unwrap();
        let meta = meta_from(&mine.join("a.md"), &raw);
        assert_eq!(meta.tags, ["mine", "shared", "theirs"]);
        assert_eq!(meta.links, ["2026-01-01-000000"]);
        assert_eq!(split_frontmatter(&raw).1, BODY, "prose is untouched");
        assert!(!mine.join("a-other-device.md").exists());

        assert_eq!(fs::read_to_string(mine.join("notes.txt")).unwrap(), "from this device\n");
        assert_eq!(
            fs::read_to_string(mine.join("notes-other-device.txt")).unwrap(),
            "from the other device\n"
        );
        assert!(git(&mine, &["status", "--porcelain"]).unwrap().trim().is_empty());
        assert_eq!(shell::push(&mine), crate::vault_git::SyncOutcome::Synced);
        cleanup([&mine, &other, &remote]);
    }

    #[test]
    fn stale_incomplete_or_dirty_resolutions_change_nothing() {
        let (mine, other, remote) = diverged("guards");
        let conflicts = list(&mine).unwrap();
        let head = git(&mine, &["rev-parse", "HEAD"]).unwrap();
        let all = choose(&conflicts, "mine");

        assert!(resolve(&mine, "stale", &conflicts.remote, &all).unwrap_err().contains("Review it again"));
        assert!(resolve(&mine, &conflicts.local, &conflicts.remote, &all[..1])
            .unwrap_err()
            .contains("every file"));
        assert!(resolve(&mine, &conflicts.local, &conflicts.remote, &choose(&conflicts, "neither")).is_err());

        write(&mine, "pending.md", "not committed yet\n");
        assert!(resolve(&mine, &conflicts.local, &conflicts.remote, &all)
            .unwrap_err()
            .contains("awaiting a commit"));

        assert_eq!(git(&mine, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(fs::read_to_string(mine.join("notes.txt")).unwrap(), "from this device\n");
        cleanup([&mine, &other, &remote]);
    }

    #[test]
    fn a_file_deleted_on_one_device_can_be_kept_or_dropped() {
        for keep in ["mine", "theirs"] {
            let remote = bare_remote(&format!("conflict-delete-{keep}-remote"));
            let mine = fresh(&format!("conflict-delete-{keep}-mine"));
            write(&mine, "a.md", &entry("", ""));
            shell::ensure_repo(&mine).unwrap();
            add_remote(&mine, &remote);
            shell::commit_all(&mine, "start").unwrap();
            shell::push(&mine);
            let other = fresh(&format!("conflict-delete-{keep}-other"));
            fs::remove_dir_all(&other).unwrap();
            git(&std::env::temp_dir(), &["clone", "--quiet", remote.to_str().unwrap(), other.to_str().unwrap()]).unwrap();
            shell::ensure_repo(&other).unwrap();
            write(&other, "a.md", &entry("kept", ""));
            shell::commit_all(&other, "tagged elsewhere").unwrap();
            shell::push(&other);
            fs::remove_file(mine.join("a.md")).unwrap();
            shell::commit_all(&mine, "deleted here").unwrap();
            shell::fetch(&mine).unwrap();

            let conflicts = list(&mine).unwrap();
            assert_eq!(conflicts.files.len(), 1);
            assert!(conflicts.files[0].mine.is_none());
            assert!(conflicts.files[0].both.is_none(), "nothing to keep on this side");
            assert!(resolve(&mine, &conflicts.local, &conflicts.remote, &choose(&conflicts, "both")).is_err());
            resolve(&mine, &conflicts.local, &conflicts.remote, &choose(&conflicts, keep)).unwrap();
            assert_eq!(mine.join("a.md").exists(), keep == "theirs");
            cleanup([&mine, &other, &remote]);
        }
    }
}
