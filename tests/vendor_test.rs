//! End-to-end test for `git_repository` (vendor a pinned snapshot): drive the
//! real binary against a local origin, pinning an exact commit SHA.

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

fn vendor(mono: &Path, origins_base: &Path, label: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_capyfun"))
        .args(["vendor", label, "--root"])
        .arg(mono)
        .env("CAPYFUN_GITHUB_BASE", origins_base)
        .output()
        .expect("run capyfun vendor")
}

fn show_main(repo: &Repository, path: &str) -> Option<String> {
    let tip = repo
        .find_reference("refs/heads/main")
        .unwrap()
        .target()
        .unwrap();
    let tree = repo.find_commit(tip).unwrap().tree().unwrap();
    let entry = tree.get_path(Path::new(path)).ok()?;
    let blob = repo.find_blob(entry.id()).unwrap();
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

fn write_git_repo_src(mono: &Path, commit: Oid) {
    std::fs::write(
        mono.join("third_party/github.com/acme/plugin/SRC"),
        format!(
            "git_repository(name = \"plugin\", repo = \"acme/plugin\", commit = \"{commit}\")\n"
        ),
    )
    .unwrap();
}

#[test]
fn vendors_pinned_snapshot_and_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let origins = tmp.path().join("origins");
    let origin_path = origins.join("acme/plugin");
    let mono_path = tmp.path().join("mono");

    // Origin with two commits; we pin the FIRST.
    let origin = init_main(&origin_path);
    let c1 = commit_files(&origin, &[("plugin.go", "v1\n"), ("README", "r\n")], "v1\n");
    let c2 = commit_files(&origin, &[("plugin.go", "v2\n"), ("README", "r\n")], "v2\n");

    // Monorepo pinning the plugin at c1.
    let mono = init_main(&mono_path);
    std::fs::create_dir_all(mono_path.join("third_party/github.com/acme/plugin")).unwrap();
    std::fs::write(
        mono_path.join("SRC"),
        "monorepo(name = \"acme\", default_branch = \"main\")\n",
    )
    .unwrap();
    write_git_repo_src(&mono_path, c1);
    {
        let mut index = mono.index().unwrap();
        index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = mono.find_tree(index.write_tree().unwrap()).unwrap();
        mono.commit(Some("HEAD"), &sig(), &sig(), "config\n", &tree, &[])
            .unwrap();
    }

    // Vendor at the pin: must materialize c1's content, not the origin tip.
    let label = "//third_party/github.com/acme/plugin:plugin";
    let out = vendor(&mono_path, &origins, label);
    assert!(
        out.status.success(),
        "vendor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        show_main(&mono, "third_party/github.com/acme/plugin/plugin.go").as_deref(),
        Some("v1\n")
    );
    // SRC metadata preserved in the dest.
    assert!(show_main(&mono, "third_party/github.com/acme/plugin/SRC")
        .unwrap()
        .contains("git_repository"));

    let tip1 = mono
        .find_reference("refs/heads/main")
        .unwrap()
        .target()
        .unwrap();

    // Re-vendor at the same pin: no-op.
    assert!(vendor(&mono_path, &origins, label).status.success());
    assert_eq!(
        mono.find_reference("refs/heads/main")
            .unwrap()
            .target()
            .unwrap(),
        tip1,
        "re-vendor at same pin must be a no-op"
    );

    // Bump the pin to c2 and re-vendor: snapshot updates.
    write_git_repo_src(&mono_path, c2);
    let out = vendor(&mono_path, &origins, label);
    assert!(
        out.status.success(),
        "re-pin failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        show_main(&mono, "third_party/github.com/acme/plugin/plugin.go").as_deref(),
        Some("v2\n")
    );
}
