//! Tests for the SRC + library config evaluator.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// Build a hermetic monorepo tree from `(relative_path, contents)` pairs and
/// return its root. Parent directories are created as needed.
fn write_tree(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for (rel, contents) in files {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
    }
    (dir, root)
}

fn imports(cfg: &RawConfig) -> Vec<&ImportDecl> {
    cfg.decls
        .iter()
        .filter_map(|d| match d {
            Decl::Import(i) => Some(i),
            _ => None,
        })
        .collect()
}

#[test]
fn evaluates_example_monorepo() {
    // The committed example must evaluate cleanly through discovery + load().
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/monorepo");
    let cfg = evaluate(&root).expect("example monorepo evaluates");

    // Exactly one monorepo singleton, declared by the root package.
    let monorepos: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Monorepo(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(monorepos.len(), 1);
    assert_eq!(monorepos[0].name, "tinytree");
    assert_eq!(monorepos[0].default_branch, "main");
    assert_eq!(monorepos[0].package, "//");

    // The `vendored` macro expanded to a single github_import.
    let imports = imports(&cfg);
    assert_eq!(imports.len(), 1);
    let backend = imports[0];
    assert_eq!(backend.name, "backend");
    assert_eq!(backend.repo, "acme/backend");
    assert_eq!(backend.git_ref, "refs/heads/main");
    assert_eq!(backend.into, None);
    assert_eq!(backend.package, "//third_party/backend");
    assert_eq!(
        backend.patches,
        vec![
            "patches/0001-pin-go-toolchain.patch",
            "patches/0002-drop-internal-telemetry.patch",
        ]
    );
}

#[test]
fn macro_in_library_expands_under_caller_package() {
    let (_dir, root) = write_tree(&[
        ("SRC", "monorepo(name = \"m\", default_branch = \"main\")\n"),
        (
            "lib/gh.scl",
            "def vendored(name, repo):\n    github_import(name = name, repo = repo)\n",
        ),
        (
            "third_party/x/SRC",
            "load(\"//lib/gh.scl\", \"vendored\")\nvendored(name = \"x\", repo = \"acme/x\")\n",
        ),
    ]);
    let cfg = evaluate(&root).unwrap();
    let imports = imports(&cfg);
    assert_eq!(imports.len(), 1);
    // Declaration is attributed to the SRC package that called the macro, not
    // the library that defined it.
    assert_eq!(imports[0].package, "//third_party/x");
    assert_eq!(imports[0].git_ref, "refs/heads/main"); // builtin default
}

#[test]
fn explicit_into_and_export_are_captured() {
    let (_dir, root) = write_tree(&[(
        "SRC",
        "monorepo(name = \"m\", default_branch = \"main\")\n\
         github_import(name = \"a\", repo = \"o/a\", ref = \"refs/heads/dev\", into = \"sub\")\n\
         github_export(name = \"e\", repo = \"o/sdk\", branch = \"release\", from_path = \"pub\")\n",
    )]);
    let cfg = evaluate(&root).unwrap();

    let imp = imports(&cfg);
    assert_eq!(imp.len(), 1);
    assert_eq!(imp[0].git_ref, "refs/heads/dev");
    assert_eq!(imp[0].into.as_deref(), Some("sub"));

    let exports: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Export(e) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "e");
    assert_eq!(exports[0].repo, "o/sdk");
    assert_eq!(exports[0].branch, "release");
    assert_eq!(exports[0].from_path.as_deref(), Some("pub"));
}

#[test]
fn captures_agent_tool_rules() {
    let (_dir, root) = write_tree(&[
        ("SRC", "monorepo(name = \"m\", default_branch = \"main\")\n"),
        (
            "tools/harness/SRC",
            "harness(name = \"cc\", kind = \"claude_code\", \
             plugins = [\"//tools/plugins:bazel\"], skills = [\"//tools/skills:review\"])\n",
        ),
        (
            "tools/models/SRC",
            "model(name = \"opus\", provider = \"anthropic\", id = \"claude-opus-4-8\")\n\
             model(name = \"gpt\", provider = \"openai\", id = \"gpt-5.5\", credential = \"env:OPENAI_API_KEY\")\n",
        ),
        (
            "tools/agent/SRC",
            "agent(name = \"reviewer\", harness = \"//tools/harness:cc\", model = \"//tools/models:opus\")\n",
        ),
        (
            "tools/agent/prompts/SRC",
            "prompt_template(name = \"review\", src = \"review.tmpl\")\n",
        ),
    ]);
    let cfg = evaluate(&root).unwrap();

    let harnesses: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Harness(h) => Some(h),
            _ => None,
        })
        .collect();
    assert_eq!(harnesses.len(), 1);
    assert_eq!(harnesses[0].name, "cc");
    assert_eq!(harnesses[0].kind, "claude_code");
    assert_eq!(harnesses[0].plugins, vec!["//tools/plugins:bazel"]);
    assert_eq!(harnesses[0].skills, vec!["//tools/skills:review"]);
    assert_eq!(harnesses[0].package, "//tools/harness");

    let models: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Model(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].provider, "anthropic");
    assert_eq!(models[0].credential, None);
    assert_eq!(models[1].credential.as_deref(), Some("env:OPENAI_API_KEY"));

    let agents: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Agent(a) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].harness, "//tools/harness:cc");
    assert_eq!(agents[0].model, "//tools/models:opus");

    let templates: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::PromptTemplate(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].src, "review.tmpl");
    assert_eq!(templates[0].package, "//tools/agent/prompts");
}

#[test]
fn agent_rule_in_library_errors() {
    let (_dir, root) = write_tree(&[
        ("SRC", "load(\"//lib/bad.scl\", \"x\")\nx()\n"),
        (
            "lib/bad.scl",
            "harness(name = \"oops\", kind = \"claude_code\")\ndef x():\n    pass\n",
        ),
    ]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cannot be instantiated in a .scl library"),
        "unexpected error: {msg}"
    );
}

#[test]
fn top_level_builtin_in_library_errors() {
    let (_dir, root) = write_tree(&[
        ("SRC", "load(\"//lib/bad.scl\", \"x\")\nx()\n"),
        (
            "lib/bad.scl",
            // Top-level builtin call in a library is forbidden.
            "github_import(name = \"oops\", repo = \"o/r\")\ndef x():\n    pass\n",
        ),
    ]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cannot be instantiated in a .scl library"),
        "unexpected error: {msg}"
    );
}

#[test]
fn missing_required_field_errors() {
    let (_dir, root) = write_tree(&[(
        "SRC",
        // `repo` is required.
        "github_import(name = \"a\")\n",
    )]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("repo"), "unexpected error: {msg}");
}

#[test]
fn unknown_field_errors() {
    let (_dir, root) = write_tree(&[(
        "SRC",
        "github_import(name = \"a\", repo = \"o/r\", bogus = 1)\n",
    )]);
    assert!(evaluate(&root).is_err());
}

#[test]
fn non_anchored_load_path_errors() {
    let (_dir, root) = write_tree(&[
        ("SRC", "load(\"lib/gh.scl\", \"vendored\")\n"),
        ("lib/gh.scl", "def vendored():\n    pass\n"),
    ]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("`//`-anchored"), "unexpected error: {msg}");
}

#[test]
fn discovery_skips_git_and_target() {
    let (_dir, root) = write_tree(&[
        ("SRC", "monorepo(name = \"m\", default_branch = \"main\")\n"),
        (".git/SRC", "this is not config\n"),
        ("target/SRC", "neither is this\n"),
        ("svc/SRC", "github_import(name = \"s\", repo = \"o/s\")\n"),
    ]);
    let found = discover_src_files(&root).unwrap();
    assert_eq!(found.len(), 2, "found: {found:?}");
    // Evaluation must not choke on the SRC files inside .git/target.
    let cfg = evaluate(&root).unwrap();
    assert_eq!(imports(&cfg).len(), 1);
}

// --- transforms ---

#[test]
fn transforms_are_captured_in_order() {
    let (_dir, root) = write_tree(&[(
        "SRC",
        "monorepo(name = \"m\", default_branch = \"main\")\n\
         github_import(\n\
         \x20   name = \"w\", repo = \"acme/w\",\n\
         \x20   transforms = [\n\
         \x20       replace(before = \"a\", after = \"b\", paths = [\"**/*.go\"]),\n\
         \x20       move(src = \"pkg\", dst = \"lib\"),\n\
         \x20       copy(src = \"x\", dst = \"y\"),\n\
         \x20       rewrite_message(strip_trailers = [\"Internal-Review\"]),\n\
         \x20   ],\n\
         )\n",
    )]);
    let cfg = evaluate(&root).unwrap();
    let imp = imports(&cfg);
    assert_eq!(imp.len(), 1);
    let ts = &imp[0].transforms;
    assert_eq!(ts.len(), 4);
    assert!(matches!(
        &ts[0],
        TransformSpec::Replace { before, after, paths, regex: false }
            if before == "a" && after == "b" && paths == &vec!["**/*.go".to_string()]
    ));
    assert!(matches!(&ts[1], TransformSpec::Move { src, dst } if src == "pkg" && dst == "lib"));
    assert!(matches!(&ts[2], TransformSpec::Copy { src, dst } if src == "x" && dst == "y"));
    assert!(matches!(
        &ts[3],
        TransformSpec::RewriteMessage { strip_trailers, .. } if strip_trailers == &vec!["Internal-Review".to_string()]
    ));
}

#[test]
fn transform_constructors_are_usable_in_a_library() {
    // Value constructors record nothing, so a `.scl` library may build transform
    // constants and an SRC may load and attach them.
    let (_dir, root) = write_tree(&[
        ("SRC", "monorepo(name = \"m\", default_branch = \"main\")\n"),
        (
            "lib/t.scl",
            "SCRUB = replace(before = \"internal/\", after = \"\", paths = [\"**/*.go\"])\n",
        ),
        (
            "third_party/x/SRC",
            "load(\"//lib/t.scl\", \"SCRUB\")\n\
             github_import(name = \"x\", repo = \"acme/x\", transforms = [SCRUB])\n",
        ),
    ]);
    let cfg = evaluate(&root).unwrap();
    let imp = imports(&cfg);
    assert_eq!(imp.len(), 1);
    assert_eq!(imp[0].transforms.len(), 1);
    assert!(matches!(
        &imp[0].transforms[0],
        TransformSpec::Replace { .. }
    ));
}

#[test]
fn non_transform_in_transforms_list_errors() {
    let (_dir, root) = write_tree(&[(
        "SRC",
        "github_import(name = \"x\", repo = \"acme/x\", transforms = [\"oops\"])\n",
    )]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("not a transform"), "unexpected error: {msg}");
}

#[test]
fn captures_on_issue_reaction() {
    let (_dir, root) = write_tree(&[(
        "automation/SRC",
        "on_issue(\n\
         \x20   name = \"prototype\",\n\
         \x20   repo = \"acme/backend\",\n\
         \x20   action = \"labeled\",\n\
         \x20   label = \"assign-agent\",\n\
         \x20   agent = \"//tools/agent:reviewer\",\n\
         \x20   prompt = template(\"//tools/agent/prompts:proto\", vars = {\"k\": \"v\"}),\n\
         )\n",
    )]);
    let cfg = evaluate(&root).unwrap();
    let reactions: Vec<_> = cfg
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::OnIssue(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(reactions.len(), 1);
    let r = reactions[0];
    assert_eq!(r.name, "prototype");
    assert_eq!(r.repo, "acme/backend");
    assert_eq!(r.action.as_deref(), Some("labeled"));
    assert_eq!(r.label.as_deref(), Some("assign-agent"));
    assert_eq!(r.agent, "//tools/agent:reviewer");
    assert_eq!(r.prompt.template, "//tools/agent/prompts:proto");
    assert_eq!(r.prompt.vars, vec![("k".to_owned(), "v".to_owned())]);
    assert_eq!(r.package, "//automation");
}

#[test]
fn on_issue_in_library_errors() {
    let (_dir, root) = write_tree(&[
        ("SRC", "load(\"//lib/bad.scl\", \"x\")\nx()\n"),
        (
            "lib/bad.scl",
            "on_issue(name = \"o\", repo = \"a/b\", agent = \"//a:b\", \
             prompt = template(\"//a:p\"))\ndef x():\n    pass\n",
        ),
    ]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cannot be instantiated in a .scl library"),
        "unexpected error: {msg}"
    );
}

#[test]
fn on_issue_non_template_prompt_errors() {
    let (_dir, root) = write_tree(&[(
        "SRC",
        "on_issue(name = \"o\", repo = \"a/b\", agent = \"//a:b\", prompt = \"oops\")\n",
    )]);
    let err = evaluate(&root).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("not a template"), "unexpected error: {msg}");
}
