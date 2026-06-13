//! Tests for the tree-prefix rewrite core, on hermetic in-memory repos.

use std::collections::BTreeMap;

use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};

use super::*;

/// Filemode for a regular file blob.
const FILEMODE_BLOB: i32 = 0o100644;

fn temp_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    (dir, repo)
}

/// Build a tree from `(path, contents)` pairs.
fn write_tree(repo: &Repository, files: &[(&str, &str)]) -> Oid {
    #[derive(Default)]
    struct Dir {
        files: BTreeMap<String, String>,
        dirs: BTreeMap<String, Dir>,
    }
    fn insert(d: &mut Dir, parts: &[&str], content: &str) {
        if parts.len() == 1 {
            d.files.insert(parts[0].to_owned(), content.to_owned());
        } else {
            insert(
                d.dirs.entry(parts[0].to_owned()).or_default(),
                &parts[1..],
                content,
            );
        }
    }
    fn build(repo: &Repository, d: &Dir) -> Oid {
        let mut b = repo.treebuilder(None).unwrap();
        for (name, content) in &d.files {
            let oid = repo.blob(content.as_bytes()).unwrap();
            b.insert(name, oid, FILEMODE_BLOB).unwrap();
        }
        for (name, sub) in &d.dirs {
            let oid = build(repo, sub);
            b.insert(name, oid, FILEMODE_TREE).unwrap();
        }
        b.write().unwrap()
    }
    let mut root = Dir::default();
    for (p, c) in files {
        let parts: Vec<&str> = p.split('/').collect();
        insert(&mut root, &parts, c);
    }
    build(repo, &root)
}

/// Flatten a tree into a sorted `path -> contents` map of its blobs.
fn read_tree(repo: &Repository, oid: Oid) -> BTreeMap<String, String> {
    let tree = repo.find_tree(oid).unwrap();
    let mut out = BTreeMap::new();
    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            let path = format!("{root}{}", entry.name().unwrap());
            let blob = repo.find_blob(entry.id()).unwrap();
            out.insert(path, String::from_utf8_lossy(blob.content()).into_owned());
        }
        TreeWalkResult::Ok
    })
    .unwrap();
    out
}

#[test]
fn splices_into_empty_base() {
    let (_d, repo) = temp_repo();
    let base = empty_tree(&repo).unwrap();
    let sub = write_tree(&repo, &[("main.go", "package main"), ("sub/x.txt", "x")]);

    let result = splice_tree(&repo, base, "third_party/backend", sub).unwrap();
    let files = read_tree(&repo, result);

    assert_eq!(
        files,
        BTreeMap::from([
            (
                "third_party/backend/main.go".to_owned(),
                "package main".to_owned()
            ),
            ("third_party/backend/sub/x.txt".to_owned(), "x".to_owned()),
        ])
    );
}

#[test]
fn preserves_unrelated_base_entries() {
    let (_d, repo) = temp_repo();
    let base = write_tree(
        &repo,
        &[
            ("README", "hi"),
            ("third_party/other/y", "y"),
            ("svc/app.go", "app"),
        ],
    );
    let sub = write_tree(&repo, &[("main.go", "m")]);

    let result = splice_tree(&repo, base, "third_party/backend", sub).unwrap();
    let files = read_tree(&repo, result);

    // The imported subtree is present...
    assert_eq!(files.get("third_party/backend/main.go").unwrap(), "m");
    // ...and everything else is untouched, including the sibling under the
    // shared `third_party/` parent.
    assert_eq!(files.get("README").unwrap(), "hi");
    assert_eq!(files.get("third_party/other/y").unwrap(), "y");
    assert_eq!(files.get("svc/app.go").unwrap(), "app");
}

#[test]
fn replaces_existing_destination() {
    let (_d, repo) = temp_repo();
    let base = write_tree(
        &repo,
        &[("third_party/backend/old.go", "old"), ("keep", "keep")],
    );
    let sub = write_tree(&repo, &[("new.go", "new")]);

    let result = splice_tree(&repo, base, "third_party/backend", sub).unwrap();
    let files = read_tree(&repo, result);

    assert!(!files.contains_key("third_party/backend/old.go"));
    assert_eq!(files.get("third_party/backend/new.go").unwrap(), "new");
    assert_eq!(files.get("keep").unwrap(), "keep");
}

#[test]
fn single_component_destination() {
    let (_d, repo) = temp_repo();
    let base = write_tree(&repo, &[("a", "a")]);
    let sub = write_tree(&repo, &[("f", "f")]);
    let result = splice_tree(&repo, base, "vendor", sub).unwrap();
    let files = read_tree(&repo, result);
    assert_eq!(files.get("vendor/f").unwrap(), "f");
    assert_eq!(files.get("a").unwrap(), "a");
}

#[test]
fn is_deterministic() {
    let (_d, repo) = temp_repo();
    let base = write_tree(&repo, &[("README", "hi")]);
    let sub = write_tree(&repo, &[("main.go", "m")]);
    let a = splice_tree(&repo, base, "third_party/backend", sub).unwrap();
    let b = splice_tree(&repo, base, "third_party/backend", sub).unwrap();
    assert_eq!(a, b, "identical inputs must yield identical tree OIDs");
}

#[test]
fn empty_destination_is_rejected() {
    let (_d, repo) = temp_repo();
    let base = empty_tree(&repo).unwrap();
    let sub = empty_tree(&repo).unwrap();
    assert!(splice_tree(&repo, base, "", sub).is_err());
    assert!(splice_tree(&repo, base, "/", sub).is_err());
}
