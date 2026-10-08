use super::*;

const SAMPLE: &str = "---\ncreated: 2026-09-21T01:08:07+05:30\ntags: []\nlinks: []\n---\n\nWorking fine\n\nLooks good\n";

fn scratch(name: &str, contents: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ff-test-{}.md", name));
    fs::write(&p, contents).unwrap();
    p
}

#[test]
fn tagging_leaves_the_body_byte_for_byte() {
    let p = scratch("body", SAMPLE);
    let (_, before) = split_frontmatter(&fs::read_to_string(&p).unwrap());

    rewrite_meta(&p, Some(vec!["rivers".into(), "tooling".into()]), None).unwrap();

    let (_, after) = split_frontmatter(&fs::read_to_string(&p).unwrap());
    assert_eq!(
        before, after,
        "body must survive a metadata write untouched"
    );
}

#[test]
fn tags_and_links_round_trip() {
    let p = scratch("round", SAMPLE);
    rewrite_meta(
        &p,
        Some(vec!["alpha".into()]),
        Some(vec!["2026-01-01-000000".into()]),
    )
    .unwrap();

    let meta = meta_from(&p, &fs::read_to_string(&p).unwrap());
    assert_eq!(meta.tags, vec!["alpha".to_string()]);
    assert_eq!(meta.links, vec!["2026-01-01-000000".to_string()]);
}

#[test]
fn unknown_frontmatter_keys_are_preserved() {
    let raw = "---\ncreated: 2026-09-21T01:08:07+05:30\nmood: restless\ntags: []\n---\n\nbody\n";
    let p = scratch("unknown", raw);
    rewrite_meta(&p, Some(vec!["x".into()]), None).unwrap();

    let out = fs::read_to_string(&p).unwrap();
    assert!(
        out.contains("mood: restless"),
        "foreign keys must not be dropped"
    );
    assert!(out.contains("created: 2026-09-21T01:08:07+05:30"));
}

#[test]
fn prose_that_looks_like_frontmatter_is_not_rewritten() {
    let raw = "---\ncreated: x\ntags: []\n---\n\ntags: this is prose, not metadata\n";
    let p = scratch("lookalike", raw);
    rewrite_meta(&p, Some(vec!["real".into()]), None).unwrap();

    let out = fs::read_to_string(&p).unwrap();
    assert!(out.contains("tags: this is prose, not metadata"));
    assert!(out.contains("tags: [real]"));
}

#[test]
fn repeated_writes_are_stable() {
    let p = scratch("stable", SAMPLE);
    rewrite_meta(&p, Some(vec!["a".into()]), None).unwrap();
    let once = fs::read_to_string(&p).unwrap();
    rewrite_meta(&p, Some(vec!["a".into()]), None).unwrap();
    let twice = fs::read_to_string(&p).unwrap();
    assert_eq!(
        once, twice,
        "rewriting the same metadata must be idempotent"
    );
}

#[test]
fn tags_are_normalised() {
    assert_eq!(clean_tag("  #Rivers "), "rivers");
    assert_eq!(clean_tag("Half[Baked]"), "halfbaked");
    assert_eq!(clean_tag("a,b"), "ab");
}

#[test]
fn dedupe_drops_repeats_and_blanks() {
    let got = dedupe(vec!["a".into(), "".into(), "a".into(), "b".into()]);
    assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
}

fn meta(name: &str, links: &[&str]) -> EntryMeta {
    EntryMeta {
        path: format!("/tmp/{}.md", name),
        name: name.into(),
        notebook: String::new(),
        created: name.into(),
        words: 0,
        preview: String::new(),
        tags: vec![],
        links: links.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn linking_is_symmetric_and_reversible() {
    let a = scratch("link-a", SAMPLE);
    let b = scratch("link-b", SAMPLE);
    let (na, nb) = (stem_of(&a), stem_of(&b));

    add_link(&a, &nb).unwrap();
    add_link(&b, &na).unwrap();
    assert_eq!(
        meta_from(&a, &fs::read_to_string(&a).unwrap()).links,
        vec![nb.clone()]
    );
    assert_eq!(
        meta_from(&b, &fs::read_to_string(&b).unwrap()).links,
        vec![na.clone()]
    );

    remove_link(&a, &nb).unwrap();
    remove_link(&b, &na).unwrap();
    assert!(meta_from(&a, &fs::read_to_string(&a).unwrap())
        .links
        .is_empty());
    assert!(meta_from(&b, &fs::read_to_string(&b).unwrap())
        .links
        .is_empty());
}

#[test]
fn linking_twice_does_not_duplicate() {
    let a = scratch("dup-a", SAMPLE);
    add_link(&a, "somewhere").unwrap();
    add_link(&a, "somewhere").unwrap();
    assert_eq!(
        meta_from(&a, &fs::read_to_string(&a).unwrap()).links.len(),
        1
    );
}

#[test]
fn related_includes_backlinks_from_a_half_written_pair() {
    let all = vec![meta("one", &["two"]), meta("two", &[]), meta("three", &[])];

    // Forward direction: one -> two.
    let from_one: Vec<_> = related_to(&all, &all[0]).iter().map(|e| &e.name).collect();
    assert_eq!(from_one, vec!["two"]);

    // Two records nothing, but must still see one.
    let from_two: Vec<_> = related_to(&all, &all[1]).iter().map(|e| &e.name).collect();
    assert_eq!(
        from_two,
        vec!["one"],
        "a one-sided link must surface on both sides"
    );

    assert!(related_to(&all, &all[2]).is_empty());
}

#[test]
fn an_entry_is_never_related_to_itself() {
    let all = vec![meta("solo", &["solo"])];
    assert!(related_to(&all, &all[0]).is_empty());
}

#[test]
fn empty_list_parses_to_nothing() {
    assert!(parse_list("[]").is_empty());
    assert_eq!(parse_list("[a, b]"), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(render_list(&["a".into(), "b".into()]), "[a, b]");
}

#[test]
fn reciprocal_links_are_cleaned_up_on_entry_removal() {
    let dir = std::env::temp_dir().join(format!("ff-del-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let a_path = dir.join("2026-09-21-000001.md");
    let b_path = dir.join("2026-09-21-000002.md");
    let na = stem_of(&a_path);
    let nb = stem_of(&b_path);

    fs::write(
        &a_path,
        format!(
            "---\ncreated: x\ntags: []\nlinks: [{}]\n---\n\nEntry A\n",
            nb
        ),
    )
    .unwrap();
    fs::write(
        &b_path,
        format!(
            "---\ncreated: x\ntags: []\nlinks: [{}]\n---\n\nEntry B\n",
            na
        ),
    )
    .unwrap();

    // Simulate deleting A:
    for other in collect_entries(&dir) {
        if other.name != na && other.links.contains(&na) {
            let p = PathBuf::from(&other.path);
            let _ = remove_link(&p, &na);
        }
    }
    fs::remove_file(&a_path).unwrap();

    assert!(!a_path.exists());
    let b_meta = meta_from(&b_path, &fs::read_to_string(&b_path).unwrap());
    assert!(
        b_meta.links.is_empty(),
        "Entry B's reciprocal link to A must be removed"
    );

    fs::remove_dir_all(&dir).unwrap();
}
