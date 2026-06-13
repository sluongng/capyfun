//! Tests for the tree-prefix rewrite core, on hermetic in-memory repos.

use std::collections::BTreeMap;

use git2::{
    DiffFormat, ObjectType, Oid, Repository, Signature, Time, TreeWalkMode, TreeWalkResult,
};

use super::*;

/// Render a unified-diff patch between two (sub)trees, as `git format-patch`-ish
/// bytes the engine can re-apply. Used to generate guaranteed-valid fixtures.
fn make_patch(repo: &Repository, old: Oid, new: Oid) -> Vec<u8> {
    let old_tree = repo.find_tree(old).unwrap();
    let new_tree = repo.find_tree(new).unwrap();
    let diff = repo
        .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)
        .unwrap();
    let mut buf = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        // Content lines carry an origin marker that must be re-prepended; file
        // and hunk headers already include their full text.
        if matches!(line.origin(), '+' | '-' | ' ') {
            buf.push(line.origin() as u8);
        }
        buf.extend_from_slice(line.content());
        true
    })
    .unwrap();
    buf
}

/// Filemode for a regular file blob.
const FILEMODE_BLOB: i32 = 0o100644;

fn temp_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    (dir, repo)
}

/// A fixed signature so commit OIDs are deterministic across runs.
fn sig(name: &str, secs: i64) -> Signature<'static> {
    Signature::new(name, &format!("{name}@example.com"), &Time::new(secs, 0)).unwrap()
}

/// Create a commit (no ref update) from a tree, with distinct author/committer
/// so preservation can be asserted.
fn commit(repo: &Repository, tree: Oid, message: &str, parents: &[Oid]) -> Oid {
    let tree = repo.find_tree(tree).unwrap();
    let parent_commits: Vec<_> = parents
        .iter()
        .map(|p| repo.find_commit(*p).unwrap())
        .collect();
    let parent_refs: Vec<&git2::Commit> = parent_commits.iter().collect();
    repo.commit(
        None,
        &sig("Author", 1000),
        &sig("Committer", 2000),
        message,
        &tree,
        &parent_refs,
    )
    .unwrap()
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

// --- M4: single-commit import (mirror layer) ---

#[test]
fn replay_first_commit_into_dest() {
    let (_d, repo) = temp_repo();
    let origin_tree = write_tree(
        &repo,
        &[("main.go", "package main"), ("go.mod", "module x")],
    );
    let origin = commit(&repo, origin_tree, "add server\n", &[]);

    let mirror = replay_commit(&repo, "third_party/backend", origin, None).unwrap();
    let mc = repo.find_commit(mirror).unwrap();

    // Tree: origin content nested under the destination prefix.
    let files = read_tree(&repo, mc.tree().unwrap().id());
    assert_eq!(
        files.get("third_party/backend/main.go").unwrap(),
        "package main"
    );
    assert_eq!(files.get("third_party/backend/go.mod").unwrap(), "module x");

    // Message: original subject + origin trailer.
    let msg = mc.message().unwrap();
    assert!(msg.starts_with("add server"));
    assert_eq!(
        parse_origin_trailer(msg).as_deref(),
        Some(origin.to_string().as_str())
    );

    // Metadata preserved; first commit has no parent.
    assert_eq!(mc.author().name().unwrap(), "Author");
    assert_eq!(mc.committer().name().unwrap(), "Committer");
    assert_eq!(mc.parent_count(), 0);
}

#[test]
fn replay_onto_parent_links_and_preserves_base() {
    let (_d, repo) = temp_repo();
    // A pre-existing monorepo commit with unrelated content.
    let base_tree = write_tree(&repo, &[("README", "root")]);
    let parent = commit(&repo, base_tree, "monorepo root\n", &[]);

    let origin_tree = write_tree(&repo, &[("lib.go", "lib")]);
    let origin = commit(&repo, origin_tree, "init lib\n", &[]);

    let mirror = replay_commit(&repo, "third_party/lib", origin, Some(parent)).unwrap();
    let mc = repo.find_commit(mirror).unwrap();

    assert_eq!(mc.parent_count(), 1);
    assert_eq!(mc.parent_id(0).unwrap(), parent);

    let files = read_tree(&repo, mc.tree().unwrap().id());
    assert_eq!(files.get("README").unwrap(), "root"); // base preserved
    assert_eq!(files.get("third_party/lib/lib.go").unwrap(), "lib");
}

#[test]
fn replay_is_deterministic() {
    let (_d, repo) = temp_repo();
    let origin_tree = write_tree(&repo, &[("f", "f")]);
    let origin = commit(&repo, origin_tree, "c\n", &[]);
    let a = replay_commit(&repo, "vendor/x", origin, None).unwrap();
    let b = replay_commit(&repo, "vendor/x", origin, None).unwrap();
    assert_eq!(a, b);
}

#[test]
fn origin_trailer_round_trips() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let msg = with_mirror_trailers(
        "subject line\n\nbody paragraph\n",
        Oid::from_str(oid).unwrap(),
        "third_party/x",
    );
    assert!(msg.contains("subject line"));
    assert!(msg.contains("body paragraph"));
    assert_eq!(parse_origin_trailer(&msg).as_deref(), Some(oid));
    assert_eq!(
        trailer_value(&msg, IMPORT_TRAILER).as_deref(),
        Some("third_party/x")
    );
    // No trailer present.
    assert_eq!(parse_origin_trailer("just a message"), None);
}

// --- M5: incremental import (mirror layer) ---

/// Walk a mirror tip's first-parent chain, returning each commit's origin
/// trailer SHA (newest first).
fn mirror_origins(repo: &Repository, head: Oid) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Some(head);
    while let Some(c) = cur {
        let commit = repo.find_commit(c).unwrap();
        if let Some(sha) = parse_origin_trailer(commit.message().unwrap()) {
            out.push(sha);
        }
        cur = commit.parent_ids().next();
    }
    out
}

#[test]
fn imports_full_history_then_is_idempotent() {
    let (_d, repo) = temp_repo();
    let c1 = commit(&repo, write_tree(&repo, &[("a", "1")]), "c1\n", &[]);
    let c2 = commit(&repo, write_tree(&repo, &[("a", "2")]), "c2\n", &[c1]);
    let c3 = commit(&repo, write_tree(&repo, &[("a", "3")]), "c3\n", &[c2]);

    let first = import_mirror(&repo, "third_party/x", c3, None).unwrap();
    assert_eq!(first.imported, 3);
    let head = first.head.unwrap();

    // Three linear mirror commits, oldest->newest mapping c1,c2,c3.
    assert_eq!(
        mirror_origins(&repo, head),
        vec![c3.to_string(), c2.to_string(), c1.to_string()]
    );
    let tip_files = read_tree(&repo, repo.find_commit(head).unwrap().tree().unwrap().id());
    assert_eq!(tip_files.get("third_party/x/a").unwrap(), "3");

    // Re-running with the same origin tip imports nothing.
    let again = import_mirror(&repo, "third_party/x", c3, Some(head)).unwrap();
    assert_eq!(again.imported, 0);
    assert_eq!(again.head, Some(head));
}

#[test]
fn imports_only_the_delta() {
    let (_d, repo) = temp_repo();
    let c1 = commit(&repo, write_tree(&repo, &[("a", "1")]), "c1\n", &[]);
    let c2 = commit(&repo, write_tree(&repo, &[("a", "2")]), "c2\n", &[c1]);

    let first = import_mirror(&repo, "vendor", c2, None).unwrap();
    assert_eq!(first.imported, 2);
    let head = first.head.unwrap();

    // Two new upstream commits.
    let c3 = commit(&repo, write_tree(&repo, &[("a", "3")]), "c3\n", &[c2]);
    let c4 = commit(&repo, write_tree(&repo, &[("a", "4")]), "c4\n", &[c3]);

    let delta = import_mirror(&repo, "vendor", c4, Some(head)).unwrap();
    assert_eq!(delta.imported, 2, "only c3, c4 are new");
    let new_head = delta.head.unwrap();
    assert_eq!(
        mirror_origins(&repo, new_head),
        vec![
            c4.to_string(),
            c3.to_string(),
            c2.to_string(),
            c1.to_string()
        ]
    );
}

#[test]
fn linearizes_merges_to_first_parent() {
    let (_d, repo) = temp_repo();
    let c1 = commit(&repo, write_tree(&repo, &[("a", "1")]), "c1\n", &[]);
    let c2 = commit(&repo, write_tree(&repo, &[("a", "2")]), "c2\n", &[c1]);
    // A feature branch off c1, not on the first-parent chain.
    let f1 = commit(
        &repo,
        write_tree(&repo, &[("a", "1"), ("b", "feat")]),
        "f1\n",
        &[c1],
    );
    // Merge with first parent c2, second parent f1; merged tree has a=2 and b.
    let merge = commit(
        &repo,
        write_tree(&repo, &[("a", "2"), ("b", "feat")]),
        "merge\n",
        &[c2, f1],
    );

    let out = import_mirror(&repo, "lib", merge, None).unwrap();
    // First-parent chain merge->c2->c1 = 3 commits; f1 is NOT mirrored.
    assert_eq!(out.imported, 3);
    let head = out.head.unwrap();
    let origins = mirror_origins(&repo, head);
    assert!(
        !origins.contains(&f1.to_string()),
        "feature commit must not be mirrored"
    );
    // Merge content is still present (we mirror the merge's tree as-is).
    let tip_files = read_tree(&repo, repo.find_commit(head).unwrap().tree().unwrap().id());
    assert_eq!(tip_files.get("lib/b").unwrap(), "feat");
    assert_eq!(tip_files.get("lib/a").unwrap(), "2");
}

#[test]
fn diverged_history_errors() {
    let (_d, repo) = temp_repo();
    let c1 = commit(&repo, write_tree(&repo, &[("a", "1")]), "c1\n", &[]);
    let head = import_mirror(&repo, "x", c1, None).unwrap().head.unwrap();

    // A fresh origin root with no relation to c1 (force-push simulation).
    let other = commit(&repo, write_tree(&repo, &[("a", "z")]), "other\n", &[]);
    let err = import_mirror(&repo, "x", other, Some(head)).unwrap_err();
    assert!(format!("{err:#}").contains("no longer contains"), "{err:#}");
}

#[test]
fn two_imports_share_a_branch_without_conflating_maps() {
    let (_d, repo) = temp_repo();
    // Origin A (two commits) and unrelated origin B (one commit).
    let a1 = commit(&repo, write_tree(&repo, &[("a", "1")]), "a1\n", &[]);
    let a2 = commit(&repo, write_tree(&repo, &[("a", "2")]), "a2\n", &[a1]);
    let b1 = commit(&repo, write_tree(&repo, &[("b", "1")]), "b1\n", &[]);

    let head_a = import_mirror(&repo, "third_party/a", a2, None)
        .unwrap()
        .head
        .unwrap();
    // Importing B onto the same branch must NOT treat A's trailers as B's map.
    let rb = import_mirror(&repo, "third_party/b", b1, Some(head_a)).unwrap();
    assert_eq!(rb.imported, 1, "B's history is independent of A's");
    let head_b = rb.head.unwrap();

    let files = read_tree(
        &repo,
        repo.find_commit(head_b).unwrap().tree().unwrap().id(),
    );
    assert_eq!(files.get("third_party/a/a").unwrap(), "2");
    assert_eq!(files.get("third_party/b/b").unwrap(), "1");

    // Re-importing A is still idempotent even though B sits on top of it.
    let again = import_mirror(&repo, "third_party/a", a2, Some(head_b)).unwrap();
    assert_eq!(again.imported, 0);
}

// --- M6: patch layer (tip, rebased on the mirror tip) ---

#[test]
fn apply_patch_modifies_subtree_under_dest() {
    let (_d, repo) = temp_repo();
    let sub_a = write_tree(&repo, &[("go.mod", "module x\n\ngo 1.21\n")]);
    let sub_b = write_tree(
        &repo,
        &[("go.mod", "module x\n\ngo 1.21\ntoolchain go1.21.6\n")],
    );
    let patch = make_patch(&repo, sub_a, sub_b);

    let base = splice_tree(
        &repo,
        empty_tree(&repo).unwrap(),
        "third_party/backend",
        sub_a,
    )
    .unwrap();
    let patched = apply_patch_to_tree(&repo, base, "third_party/backend", &patch).unwrap();
    let files = read_tree(&repo, patched);
    assert!(files
        .get("third_party/backend/go.mod")
        .unwrap()
        .contains("toolchain go1.21.6"));
}

#[test]
fn patch_layer_stacks_commits_with_trailers() {
    let (_d, repo) = temp_repo();
    // Mirror tip: one commit with go.mod under dest.
    let sub0 = write_tree(&repo, &[("go.mod", "module x\n\ngo 1.21\n")]);
    let origin = commit(&repo, sub0, "init\n", &[]);
    let mirror = import_mirror(&repo, "third_party/backend", origin, None)
        .unwrap()
        .head
        .unwrap();

    // Patch 1: add toolchain. Patch 2: add a README.
    let sub1 = write_tree(
        &repo,
        &[("go.mod", "module x\n\ngo 1.21\ntoolchain go1.21.6\n")],
    );
    let p1 = make_patch(&repo, sub0, sub1);
    let sub2 = write_tree(
        &repo,
        &[
            ("go.mod", "module x\n\ngo 1.21\ntoolchain go1.21.6\n"),
            ("README", "hi\n"),
        ],
    );
    let p2 = make_patch(&repo, sub1, sub2);

    let patches = vec![
        PatchFile {
            label: "patches/0001.patch".into(),
            bytes: p1,
        },
        PatchFile {
            label: "patches/0002.patch".into(),
            bytes: p2,
        },
    ];
    let tip = apply_patch_layer(&repo, "third_party/backend", mirror, &patches).unwrap();

    // Two patch commits on top of the mirror.
    let tip_commit = repo.find_commit(tip).unwrap();
    assert!(has_trailer(tip_commit.message().unwrap(), PATCH_TRAILER));
    assert_eq!(tip_commit.parent_id(0).unwrap(), {
        // parent is the first patch commit, whose parent is the mirror tip
        let p = repo.find_commit(tip_commit.parent_id(0).unwrap()).unwrap();
        assert_eq!(p.parent_id(0).unwrap(), mirror);
        p.id()
    });

    let files = read_tree(&repo, tip_commit.tree().unwrap().id());
    assert!(files
        .get("third_party/backend/go.mod")
        .unwrap()
        .contains("toolchain"));
    assert_eq!(files.get("third_party/backend/README").unwrap(), "hi\n");
}

#[test]
fn failing_patch_aborts_without_moving_state() {
    let (_d, repo) = temp_repo();
    let sub0 = write_tree(&repo, &[("go.mod", "module x\n\ngo 1.21\n")]);
    let origin = commit(&repo, sub0, "init\n", &[]);
    let mirror = import_mirror(&repo, "dst", origin, None)
        .unwrap()
        .head
        .unwrap();

    // A patch that targets content not present -> won't apply.
    let bogus = b"diff --git a/go.mod b/go.mod\n--- a/go.mod\n+++ b/go.mod\n@@ -1,1 +1,1 @@\n-nonexistent line\n+replacement\n".to_vec();
    let patches = vec![PatchFile {
        label: "bad.patch".into(),
        bytes: bogus,
    }];
    let err = apply_patch_layer(&repo, "dst", mirror, &patches).unwrap_err();
    assert!(format!("{err:#}").contains("bad.patch"), "{err:#}");
    // Mirror tip is untouched (no ref was moved by the engine).
    assert!(repo.find_commit(mirror).is_ok());
}

#[test]
fn import_with_patches_is_idempotent_and_rebases() {
    let (_d, repo) = temp_repo();
    // A file large enough that the patch's region and a later upstream change in
    // a different region do not overlap (so the patch rebases cleanly).
    let v1 = "module acme/widget\n\ngo 1.21\n\n// pad a\n// pad b\n// pad c\n// pad d\n\nrequire cobra v1.8.0\n";
    let v1_patched = "module acme/widget\n\ngo 1.21\ntoolchain go1.21.6\n\n// pad a\n// pad b\n// pad c\n// pad d\n\nrequire cobra v1.8.0\n";
    let sub_v1 = write_tree(&repo, &[("go.mod", v1)]);
    let c1 = commit(&repo, sub_v1, "c1\n", &[]);

    // A patch adding a toolchain line near the top of v1.
    let sub_v1_patched = write_tree(&repo, &[("go.mod", v1_patched)]);
    let patch = PatchFile {
        label: "patches/0001.patch".into(),
        bytes: make_patch(&repo, sub_v1, sub_v1_patched),
    };

    let first = import(&repo, "dst", c1, None, std::slice::from_ref(&patch)).unwrap();
    assert_eq!(first.imported, 1);
    let tip1 = first.head.unwrap();

    // Re-run, same upstream + same patch: nothing new, identical tip OID.
    let again = import(&repo, "dst", c1, Some(tip1), std::slice::from_ref(&patch)).unwrap();
    assert_eq!(again.imported, 0);
    assert_eq!(
        again.head,
        Some(tip1),
        "deterministic patch layer => stable tip"
    );

    // New upstream commit changing a *different* region (the require line);
    // the patch must rebase onto the new mirror tip.
    let v2 = "module acme/widget\n\ngo 1.21\n\n// pad a\n// pad b\n// pad c\n// pad d\n\nrequire cobra v1.9.0\n";
    let sub_v2 = write_tree(&repo, &[("go.mod", v2)]);
    let c2 = commit(&repo, sub_v2, "bump cobra\n", &[c1]);
    let third = import(&repo, "dst", c2, Some(tip1), std::slice::from_ref(&patch)).unwrap();
    assert_eq!(third.imported, 1, "only c2 is new");
    let tip2 = third.head.unwrap();

    // Tip reflects c2 content plus the rebased patch.
    let files = read_tree(&repo, repo.find_commit(tip2).unwrap().tree().unwrap().id());
    let go_mod = files.get("dst/go.mod").unwrap();
    assert!(
        go_mod.contains("require cobra v1.9.0"),
        "new upstream content: {go_mod}"
    );
    assert!(
        go_mod.contains("toolchain go1.21.6"),
        "patch rebased on top: {go_mod}"
    );

    // Top commit is a patch commit; the mirror underneath has two origin commits.
    let tip_commit = repo.find_commit(tip2).unwrap();
    assert!(has_trailer(tip_commit.message().unwrap(), PATCH_TRAILER));
    assert_eq!(
        mirror_origins(&repo, tip2),
        vec![c2.to_string(), c1.to_string()]
    );
}
