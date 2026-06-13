//! End-to-end tests for `capyfun status` and `capyfun reconcile`, driving the
//! real binary against local origin repositories (via `CAPYFUN_GITHUB_BASE`).
//! Status is the dry-run desired-vs-actual report; reconcile runs the idempotent
//! import/vendor actions to converge.

use std::path::Path;
use std::process::Command;

use git2::{IndexAddOption, Oid, Repository, Signature, Time};

fn sig() -> Signature<'static> {
    Signature::new("T", "t@example.com", &Time::new(1000, 0)).unwrap()
}

fn init_main(path: &Path) -> Repository {
    let repo = Repository::init(path).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    repo
}

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

fn run(cmd: &str, mono: &Path, origins: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec![cmd];
    args.extend_from_slice(extra);
    args.push("--root");
    Command::new(env!("CARGO_BIN_EXE_capyfun"))
        .args(&args)
        .arg(mono)
        .env("CAPYFUN_GITHUB_BASE", origins)
        .output()
        .expect("run capyfun")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The blob contents at `path` in `main`'s tip, or `None` if absent.
fn show_main(repo: &Repository, path: &str) -> Option<String> {
    let tip = repo.find_reference("refs/heads/main").ok()?.target()?;
    let tree = repo.find_commit(tip).unwrap().tree().unwrap();
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).unwrap();
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

#[test]
fn status_then_reconcile_import_and_vendor() {
    let tmp = tempfile::tempdir().unwrap();
    let origins = tmp.path().join("origins");
    let mono_path = tmp.path().join("mono");

    // --- origin: a history-tracked backend ---
    let backend = init_main(&origins.join("acme/backend"));
    commit_files(&backend, &[("go.mod", "module acme/backend\n")], "init\n");

    // --- origin: a pinned widget; capture its commit for the vendor pin ---
    let widget = init_main(&origins.join("acme/widget"));
    let widget_sha =
        commit_files(&widget, &[("widget.go", "package widget\n")], "init widget\n").to_string();

    // --- monorepo: config declaring an import and a pinned vendor ---
    let mono = init_main(&mono_path);
    commit_files(
        &mono,
        &[
            ("SRC", "monorepo(name = \"acme\", default_branch = \"main\")\n"),
            (
                "third_party/backend/SRC",
                "github_import(name = \"backend\", repo = \"acme/backend\")\n",
            ),
            (
                "third_party/widget/SRC",
                &format!(
                    "git_repository(name = \"widget\", repo = \"acme/widget\", commit = \"{widget_sha}\")\n"
                ),
            ),
        ],
        "configure\n",
    );

    // --- status before any sync: both targets need reconcile ---
    let out = run("status", &mono_path, &origins, &[]);
    assert!(out.status.success(), "status: {}", String::from_utf8_lossy(&out.stderr));
    let s = stdout(&out);
    assert!(s.contains("//third_party/backend:backend  import"), "{s}");
    assert!(s.contains("//third_party/widget:widget  vendor"), "{s}");
    assert!(s.contains("uninitialized; 1 commit(s) behind"), "import behind: {s}");
    assert!(s.contains("uninitialized; pin to vendor"), "vendor uninitialized: {s}");
    assert!(s.contains("2 target(s); 2 need reconcile"), "tally: {s}");

    // --- reconcile everything ---
    let out = run("reconcile", &mono_path, &origins, &[]);
    assert!(out.status.success(), "reconcile: {}", String::from_utf8_lossy(&out.stderr));
    let s = stdout(&out);
    assert!(s.contains("imported 1 commit(s)"), "{s}");
    assert!(s.contains("vendored acme/widget@"), "{s}");
    assert!(s.contains("reconciled 2 target(s)"), "{s}");

    // Both landed under their packages.
    assert_eq!(
        show_main(&mono, "third_party/backend/go.mod").as_deref(),
        Some("module acme/backend\n")
    );
    assert_eq!(
        show_main(&mono, "third_party/widget/widget.go").as_deref(),
        Some("package widget\n")
    );

    // --- status now clean ---
    let out = run("status", &mono_path, &origins, &[]);
    let s = stdout(&out);
    assert!(s.contains("0 need reconcile"), "all up to date: {s}");

    // --- reconcile again is a no-op ---
    let out = run("reconcile", &mono_path, &origins, &[]);
    assert!(out.status.success());
    let s = stdout(&out);
    assert!(s.contains("already up to date"), "import no-op: {s}");
    assert!(s.contains("already vendored at"), "vendor no-op: {s}");

    // --- a new upstream commit: status reports the delta ---
    commit_files(&backend, &[("go.mod", "module acme/backend\n\ngo 1.22\n")], "bump go\n");
    let out = run("status", &mono_path, &origins, &["//third_party/backend:backend"]);
    let s = stdout(&out);
    assert!(s.contains("1 commit(s) behind"), "single-target delta: {s}");
    assert!(!s.contains("widget"), "single-target filter excludes vendor: {s}");

    // --- reconcile just that target picks up the delta ---
    let out = run("reconcile", &mono_path, &origins, &["//third_party/backend:backend"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("imported 1 commit(s)"), "{}", stdout(&out));
    assert_eq!(
        show_main(&mono, "third_party/backend/go.mod").as_deref(),
        Some("module acme/backend\n\ngo 1.22\n")
    );
}

#[test]
fn unknown_target_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let origins = tmp.path().join("origins");
    let mono_path = tmp.path().join("mono");
    let mono = init_main(&mono_path);
    commit_files(
        &mono,
        &[("SRC", "monorepo(name = \"acme\", default_branch = \"main\")\n")],
        "configure\n",
    );
    let _ = &mono;

    let out = run("status", &mono_path, &origins, &["//nope:nope"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no target labeled"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run("reconcile", &mono_path, &origins, &["//nope:nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no target labeled"));
}
