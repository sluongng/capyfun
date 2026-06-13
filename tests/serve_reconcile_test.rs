//! End-to-end proof that the automation server *acts* on a trigger: a
//! [`ReconcileActor`] built from a real IR reconciles the triggered target
//! against local origins (via `CAPYFUN_GITHUB_BASE`), landing the import.
//!
//! This lives in its own test binary because it sets the process-global
//! `CAPYFUN_GITHUB_BASE`; with a single test per process there is no race.

use std::path::Path;

use capyfun::server::{Actor, ReconcileActor, Trigger};
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

fn show_main(repo: &Repository, path: &str) -> Option<String> {
    let tip = repo.find_reference("refs/heads/main").ok()?.target()?;
    let tree = repo.find_commit(tip).unwrap().tree().unwrap();
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).unwrap();
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

#[test]
fn reconcile_actor_lands_a_triggered_import() {
    let tmp = tempfile::tempdir().unwrap();
    let origins = tmp.path().join("origins");
    let mono_path = tmp.path().join("mono");

    // Origin with one commit on main.
    let backend = init_main(&origins.join("acme/backend"));
    commit_files(&backend, &[("go.mod", "module acme/backend\n")], "init\n");

    // Monorepo declaring the import.
    let mono = init_main(&mono_path);
    commit_files(
        &mono,
        &[
            ("SRC", "monorepo(name = \"acme\", default_branch = \"main\")\n"),
            (
                "third_party/backend/SRC",
                "github_import(name = \"backend\", repo = \"acme/backend\")\n",
            ),
        ],
        "configure\n",
    );

    // Point the engine at the local origins, then build the IR + actor.
    std::env::set_var("CAPYFUN_GITHUB_BASE", &origins);
    let raw = capyfun::config::evaluate(&mono_path).unwrap();
    let ir = capyfun::ir::compile(&raw).unwrap();
    let actor = ReconcileActor::new(ir, mono_path.clone());

    // A push event for the subscribed repo/ref, as the server would synthesize.
    let trigger = Trigger {
        label: "//third_party/backend:backend".to_owned(),
        repo: "acme/backend".to_owned(),
        event: "PushEvent".to_owned(),
        git_ref: Some("refs/heads/main".to_owned()),
        sha: None,
    };

    let status = actor.act(std::slice::from_ref(&trigger));
    assert!(status.contains("1/1 target(s) reconciled"), "status: {status}");

    // The import actually landed under the package.
    assert_eq!(
        show_main(&mono, "third_party/backend/go.mod").as_deref(),
        Some("module acme/backend\n")
    );

    // Acting again is idempotent — the target is already up to date.
    let status = actor.act(std::slice::from_ref(&trigger));
    assert!(status.contains("1/1 target(s) reconciled"), "status: {status}");

    // An empty batch reconciles nothing.
    assert!(actor.act(&[]).contains("0 target(s) reconciled"));
}
