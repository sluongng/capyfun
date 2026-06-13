//! End-to-end export test: drive the real `capyfun` binary against a local
//! destination repository (via `CAPYFUN_GITHUB_BASE`), exercising evaluate -> IR
//! -> validate -> fetch destination -> project `from` subtree (prefix stripped) ->
//! push export branch. PR creation is skipped for the local destination; the
//! branch push is the testable artifact, and the `CapyFun-Export` commit map is
//! verified to make re-export idempotent.

use std::path::Path;
use std::process::Command;

use git2::{IndexAddOption, Oid, Repository, Signature, Time};

fn sig() -> Signature<'static> {
    Signature::new("Dev", "dev@example.com", &Time::new(2000, 0)).unwrap()
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

/// Commit flat root-level files into a bare repo on `refs/heads/main` (libgit2's
/// local push requires a bare destination, so the dest cannot use a worktree).
fn commit_bare(repo: &Repository, files: &[(&str, &str)], message: &str) -> Oid {
    let mut builder = repo.treebuilder(None).unwrap();
    for (name, content) in files {
        let blob = repo.blob(content.as_bytes()).unwrap();
        builder.insert(name, blob, 0o100644).unwrap();
    }
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let parents: Vec<_> = repo
        .find_reference("refs/heads/main")
        .ok()
        .and_then(|r| r.target())
        .map(|p| repo.find_commit(p).unwrap())
        .into_iter()
        .collect();
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(
        Some("refs/heads/main"),
        &sig(),
        &sig(),
        message,
        &tree,
        &parent_refs,
    )
    .unwrap()
}

/// Run `capyfun export <label> --root <mono> --no-pr` against the local dest base.
fn run_export(mono: &Path, dest_base: &Path, label: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_capyfun"))
        .args(["export", label, "--no-pr", "--root"])
        .arg(mono)
        .env("CAPYFUN_GITHUB_BASE", dest_base)
        .output()
        .expect("run capyfun")
}

/// The tip of `branch` in `repo`.
fn branch_tip(repo: &Repository, branch: &str) -> Oid {
    repo.find_reference(&format!("refs/heads/{branch}"))
        .unwrap_or_else(|_| panic!("branch {branch} exists"))
        .target()
        .unwrap()
}

/// The blob contents at `path` in `commit`'s tree.
fn show(repo: &Repository, commit: Oid, path: &str) -> String {
    let tree = repo.find_commit(commit).unwrap().tree().unwrap();
    let entry = tree
        .get_path(Path::new(path))
        .unwrap_or_else(|_| panic!("{path} present"));
    let blob = repo.find_blob(entry.id()).unwrap();
    String::from_utf8_lossy(blob.content()).into_owned()
}

#[test]
fn end_to_end_export_to_pr_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dests = tmp.path().join("dests");
    let dest_path = dests.join("acme/sdk-go");
    let mono_path = tmp.path().join("mono");

    // --- destination repo: an existing public SDK with one commit on main.
    // Bare, because libgit2's local push only targets bare repositories. ---
    let dest = Repository::init_bare(&dest_path).unwrap();
    commit_bare(&dest, &[("README.md", "# acme sdk-go\n")], "initial\n");
    let dest_initial = branch_tip(&dest, "main");

    // --- monorepo: root SRC + an export package + the SDK source under it ---
    let mono = init_main(&mono_path);
    commit_files(
        &mono,
        &[
            ("SRC", "monorepo(name = \"acme\", default_branch = \"main\")\n"),
            (
                "sdk/go/SRC",
                "github_export(name = \"go-sdk\", repo = \"acme/sdk-go\", \
                 branch = \"main\", from_path = \"client\")\n",
            ),
            ("sdk/go/client/client.go", "package client\n\nconst V = 1\n"),
            ("sdk/go/client/go.mod", "module acme/sdk-go\n"),
        ],
        "add go sdk client\n",
    );

    // --- first export ---
    let out = run_export(&mono_path, &dests, "//sdk/go:go-sdk");
    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The export branch landed on the destination with the `client` prefix
    // stripped: SDK files at the destination root, CapyFun's SRC not shipped.
    let export_tip = branch_tip(&dest, "capyfun/export-go-sdk");
    assert_eq!(
        show(&dest, export_tip, "client.go"),
        "package client\n\nconst V = 1\n"
    );
    assert_eq!(show(&dest, export_tip, "go.mod"), "module acme/sdk-go\n");
    let tree = dest.find_commit(export_tip).unwrap().tree().unwrap();
    assert!(
        tree.get_path(Path::new("SRC")).is_err(),
        "CapyFun SRC must not be exported"
    );
    assert!(
        tree.get_path(Path::new("client")).is_err(),
        "the `from` prefix must be stripped, not nested"
    );

    // The commit carries the export-side commit map trailer.
    let msg = dest
        .find_commit(export_tip)
        .unwrap()
        .message()
        .unwrap()
        .to_owned();
    assert!(msg.contains("CapyFun-Export:"), "export trailer: {msg}");
    // The export sits on top of the destination's existing history.
    assert_eq!(
        dest.find_commit(export_tip).unwrap().parent_id(0).unwrap(),
        dest_initial,
        "export must build on the destination branch tip"
    );

    // --- simulate the PR being merged: fast-forward main to the export tip ---
    dest.reference("refs/heads/main", export_tip, true, "merge export PR")
        .unwrap();

    // --- re-run with nothing new: idempotent no-op ---
    let out = run_export(&mono_path, &dests, "//sdk/go:go-sdk");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("already up to date"),
        "re-export must be a no-op: {stdout}"
    );

    // --- a new monorepo change to the exported path: delta export ---
    commit_files(
        &mono,
        &[("sdk/go/client/client.go", "package client\n\nconst V = 2\n")],
        "bump sdk client version\n",
    );
    let out = run_export(&mono_path, &dests, "//sdk/go:go-sdk");
    assert!(
        out.status.success(),
        "delta export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let new_tip = branch_tip(&dest, "capyfun/export-go-sdk");
    assert_ne!(new_tip, export_tip, "delta should advance the export branch");
    assert_eq!(
        show(&dest, new_tip, "client.go"),
        "package client\n\nconst V = 2\n"
    );
    // The delta builds on the merged destination history, not from scratch.
    assert_eq!(
        dest.find_commit(new_tip).unwrap().parent_id(0).unwrap(),
        export_tip,
        "delta export must build on the merged destination tip"
    );
}
