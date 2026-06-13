//! End-to-end import test: drive the real `capyfun` binary against a local
//! origin repository (via `CAPYFUN_GITHUB_BASE`), exercising evaluate -> IR ->
//! validate -> fetch -> mirror + patch layer -> ref update.

use std::path::Path;
use std::process::Command;

use git2::{DiffFormat, IndexAddOption, Oid, Repository, Signature, Time};

/// A go.mod large enough that the patch's region and a later upstream change in
/// a different region don't overlap.
const GO_MOD_V1: &str =
    "module acme/backend\n\ngo 1.21\n\n// pad a\n// pad b\n// pad c\n// pad d\n\nrequire cobra v1.8.0\n";
const GO_MOD_V1_PATCHED: &str = "module acme/backend\n\ngo 1.21\ntoolchain go1.21.6\n\n// pad a\n// pad b\n// pad c\n// pad d\n\nrequire cobra v1.8.0\n";
const GO_MOD_V2: &str =
    "module acme/backend\n\ngo 1.21\n\n// pad a\n// pad b\n// pad c\n// pad d\n\nrequire cobra v1.9.0\n";

fn sig() -> Signature<'static> {
    Signature::new("T", "t@example.com", &Time::new(1000, 0)).unwrap()
}

/// Initialize a repo at `path` with `main` checked out.
fn init_main(path: &Path) -> Repository {
    let repo = Repository::init(path).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    repo
}

/// Write files to the worktree and commit them on HEAD.
fn commit_files(repo: &Repository, files: &[(&str, &str)], message: &str) -> Oid {
    let root = repo.workdir().unwrap();
    for (rel, contents) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }
    let mut index = repo.index().unwrap();
    index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parents: Vec<_> = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|p| repo.find_commit(p).unwrap())
        .into_iter()
        .collect();
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig(), &sig(), message, &tree, &parent_refs)
        .unwrap()
}

/// Render a unified-diff patch between two single-file trees.
fn make_patch(repo: &Repository, filename: &str, old: &str, new: &str) -> Vec<u8> {
    let mk = |content: &str| {
        let blob = repo.blob(content.as_bytes()).unwrap();
        let mut b = repo.treebuilder(None).unwrap();
        b.insert(filename, blob, 0o100644).unwrap();
        repo.find_tree(b.write().unwrap()).unwrap()
    };
    let (old_t, new_t) = (mk(old), mk(new));
    let diff = repo
        .diff_tree_to_tree(Some(&old_t), Some(&new_t), None)
        .unwrap();
    let mut buf = Vec::new();
    diff.print(DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            buf.push(line.origin() as u8);
        }
        buf.extend_from_slice(line.content());
        true
    })
    .unwrap();
    buf
}

/// Run `capyfun import <label> --root <mono>` against the local origin base.
fn run_import(mono: &Path, origins_base: &Path, label: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_capyfun"))
        .args(["import", label, "--root"])
        .arg(mono)
        .env("CAPYFUN_GITHUB_BASE", origins_base)
        .output()
        .expect("run capyfun")
}

/// The blob contents at `path` in the commit `main` points to.
fn show_main(repo: &Repository, path: &str) -> String {
    let tip = repo
        .find_reference("refs/heads/main")
        .unwrap()
        .target()
        .unwrap();
    let tree = repo.find_commit(tip).unwrap().tree().unwrap();
    let entry = tree.get_path(Path::new(path)).unwrap();
    let blob = repo.find_blob(entry.id()).unwrap();
    String::from_utf8_lossy(blob.content()).into_owned()
}

/// Commit messages along main's first-parent chain (newest first).
fn main_messages(repo: &Repository) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = repo.find_reference("refs/heads/main").unwrap().target();
    while let Some(c) = cur {
        let commit = repo.find_commit(c).unwrap();
        out.push(commit.message().unwrap().to_owned());
        cur = commit.parent_ids().next();
    }
    out
}

#[test]
fn end_to_end_import_mirror_and_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let origins = tmp.path().join("origins");
    let origin_path = origins.join("acme/backend");
    let mono_path = tmp.path().join("mono");

    // --- origin repo: two commits on main ---
    let origin = init_main(&origin_path);
    commit_files(
        &origin,
        &[("go.mod", GO_MOD_V1), ("main.go", "package main\n")],
        "init\n",
    );

    // --- monorepo: config + a generated patch, committed on main ---
    let mono = init_main(&mono_path);
    let patch = make_patch(&mono, "go.mod", GO_MOD_V1, GO_MOD_V1_PATCHED);
    commit_files(
        &mono,
        &[
            (
                "SRC",
                "monorepo(name = \"acme\", default_branch = \"main\")\n",
            ),
            (
                "third_party/backend/SRC",
                "github_import(name = \"backend\", repo = \"acme/backend\", \
                 patches = [\"patches/0001-toolchain.patch\"])\n",
            ),
            (
                "third_party/backend/patches/0001-toolchain.patch",
                &String::from_utf8_lossy(&patch),
            ),
        ],
        "configure import\n",
    );

    // --- first import ---
    let out = run_import(&mono_path, &origins, "//third_party/backend:backend");
    assert!(
        out.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Imported source present under the package, with the patch applied.
    let go_mod = show_main(&mono, "third_party/backend/go.mod");
    assert!(
        go_mod.contains("require cobra v1.8.0"),
        "upstream content: {go_mod}"
    );
    assert!(
        go_mod.contains("toolchain go1.21.6"),
        "patch applied: {go_mod}"
    );
    assert_eq!(
        show_main(&mono, "third_party/backend/main.go"),
        "package main\n"
    );
    // CapyFun metadata in the package survived the import.
    assert!(show_main(&mono, "third_party/backend/SRC").contains("github_import"));
    assert!(
        show_main(&mono, "third_party/backend/patches/0001-toolchain.patch").contains("diff --git")
    );

    let msgs = main_messages(&mono);
    assert!(
        msgs.iter().any(|m| m.contains("CapyFun-Origin")),
        "mirror trailer: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("CapyFun-Patch")),
        "patch trailer: {msgs:?}"
    );

    let tip_after_first = repo_main(&mono);

    // --- re-run: idempotent ---
    let out = run_import(&mono_path, &origins, "//third_party/backend:backend");
    assert!(out.status.success());
    assert_eq!(
        repo_main(&mono),
        tip_after_first,
        "re-import must be a no-op"
    );

    // --- new upstream commit: delta imported, patch rebased ---
    commit_files(&origin, &[("go.mod", GO_MOD_V2)], "bump cobra\n");
    let out = run_import(&mono_path, &origins, "//third_party/backend:backend");
    assert!(
        out.status.success(),
        "delta import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        repo_main(&mono),
        tip_after_first,
        "delta should advance main"
    );

    let go_mod = show_main(&mono, "third_party/backend/go.mod");
    assert!(
        go_mod.contains("require cobra v1.9.0"),
        "new upstream: {go_mod}"
    );
    assert!(
        go_mod.contains("toolchain go1.21.6"),
        "patch rebased: {go_mod}"
    );
}

fn repo_main(repo: &Repository) -> Oid {
    repo.find_reference("refs/heads/main")
        .unwrap()
        .target()
        .unwrap()
}
