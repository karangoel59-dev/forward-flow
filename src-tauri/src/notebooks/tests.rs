use super::*;
use crate::entries::{add_link, collect_entries, next_entry_path, related_to};

const SAMPLE: &str = "---\ncreated: 2026-09-21T01:08:07+05:30\ntags: []\nlinks: []\n---\n\nWorking fine\n\nLooks good\n";

fn notebook_vault(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ff-notebook-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

#[test]
fn notebooks_include_nested_entries_and_skip_hidden_files() {
    let root = notebook_vault("discovery");
    create_notebook_folder(&root, " Work ").unwrap();
    fs::create_dir_all(root.join("Work/Ideas")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("a.md"), SAMPLE).unwrap();
    fs::write(root.join("Work/Ideas/b.md"), SAMPLE).unwrap();
    fs::write(root.join(".git/hidden.md"), SAMPLE).unwrap();
    fs::write(root.join("Work/.hidden.md"), SAMPLE).unwrap();
    assert!(root.join("Work/.gitkeep").exists());
    assert_eq!(
        collect_notebooks(&root).unwrap(),
        vec!["Work", "Work/Ideas"]
    );
    let entries = collect_entries(&root);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].notebook, "Work/Ideas");
    assert_eq!(entries[1].notebook, "");
    assert!(create_notebook_folder(&root, "Work").is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn moving_between_notebooks_preserves_bytes_and_links() {
    let root = notebook_vault("move");
    create_notebook_folder(&root, "Ideas").unwrap();
    let a = root.join("a.md");
    let b = root.join("b.md");
    fs::write(&a, SAMPLE).unwrap();
    fs::write(&b, SAMPLE).unwrap();
    add_link(&a, "b").unwrap();
    add_link(&b, "a").unwrap();
    let before = fs::read(&a).unwrap();
    let moved = move_entry_file(&root, &a, "Ideas").unwrap();
    assert_eq!(fs::read(&moved).unwrap(), before);
    assert!(!a.exists());
    let entries = collect_entries(&root);
    let a_meta = entries.iter().find(|e| e.name == "a").unwrap();
    assert_eq!(related_to(&entries, a_meta)[0].name, "b");
    assert_eq!(move_entry_file(&root, &moved, "").unwrap(), a);
    assert_eq!(fs::read(&a).unwrap(), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_entry_names_remain_unique_across_notebooks() {
    let root = notebook_vault("filenames");
    create_notebook_folder(&root, "Ideas").unwrap();
    fs::write(root.join("stamp.md"), SAMPLE).unwrap();
    fs::write(root.join("Ideas/stamp-1.md"), SAMPLE).unwrap();
    let path = next_entry_path(&root, &root.join("Ideas"), "stamp");
    assert_eq!(path, root.join("Ideas/stamp-2.md"));
    assert_eq!(fs::read_to_string(root.join("stamp.md")).unwrap(), SAMPLE);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn notebook_move_collision_keeps_both_entries() {
    let root = notebook_vault("collision");
    create_notebook_folder(&root, "Ideas").unwrap();
    let source = root.join("a.md");
    let target = root.join("Ideas/a.md");
    fs::write(&source, "first").unwrap();
    fs::write(&target, "second").unwrap();
    assert!(move_entry_file(&root, &source, "Ideas").is_err());
    assert_eq!(fs::read_to_string(source).unwrap(), "first");
    assert_eq!(fs::read_to_string(target).unwrap(), "second");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn notebook_paths_reject_traversal_and_internal_files() {
    let root = notebook_vault("validation");
    for name in ["", ".git", "../outside", "a/b", "a\\b", "bad:name"] {
        assert!(create_notebook_folder(&root, name).is_err(), "{name}");
    }
    for name in ["../", ".git", "/tmp", "a\\b"] {
        assert!(notebook_dir(&root, name).is_err(), "{name}");
    }
    fs::create_dir(root.join(".git")).unwrap();
    let internal = root.join(".git/config.md");
    fs::write(&internal, "secret").unwrap();
    assert!(move_entry_file(&root, &internal, "").is_err());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn notebook_discovery_does_not_follow_symlinks() {
    let root = notebook_vault("symlinks");
    let outside = notebook_vault("outside");
    fs::write(outside.join("a.md"), SAMPLE).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("Linked")).unwrap();
    assert!(collect_notebooks(&root).unwrap().is_empty());
    assert!(collect_entries(&root).is_empty());
    assert!(notebook_dir(&root, "Linked").is_err());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[test]
fn notebook_purpose_round_trips_without_becoming_a_page() {
    let root = notebook_vault("purpose");
    assert_eq!(read_purpose(&root).unwrap(), "");
    write_purpose(&root, "Help me think about travel").unwrap();
    assert_eq!(read_purpose(&root).unwrap(), "Help me think about travel");
    assert!(collect_entries(&root).is_empty());
    assert!(write_purpose(&root, &"x".repeat(12_001)).is_err());
    fs::remove_dir_all(root).unwrap();
}
