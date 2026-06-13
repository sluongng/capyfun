use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::engine::AgentInvocation;
use crate::forge::LocalForge;
use crate::ir::{Agent, Harness, Model, Monorepo, PromptTemplate, Reaction};

// --- parsing ---------------------------------------------------------------

fn labeled_payload() -> serde_json::Value {
    serde_json::json!({
        "action": "labeled",
        "label": { "name": "assign-agent" },
        "issue": {
            "number": 7,
            "title": "Add a /version endpoint",
            "body": "We should expose the build version.",
            "html_url": "https://github.com/acme/backend/issues/7",
            "labels": [{ "name": "enhancement" }]
        },
        "repository": { "full_name": "acme/backend", "default_branch": "main" },
        "installation": { "id": 42 }
    })
}

#[test]
fn parses_issue_webhook() {
    let ev = parse_webhook_issue(&labeled_payload()).unwrap();
    assert_eq!(ev.repo, "acme/backend");
    assert_eq!(ev.action, "labeled");
    assert_eq!(ev.number, 7);
    assert_eq!(ev.title, "Add a /version endpoint");
    assert_eq!(ev.default_branch, "main");
    assert_eq!(ev.installation_id, Some(42));
    // The labeled label is folded into the label set alongside existing labels.
    assert!(ev.labels.contains(&"assign-agent".to_owned()));
    assert!(ev.labels.contains(&"enhancement".to_owned()));
}

// --- matching --------------------------------------------------------------

fn reaction(action: Option<&str>, label: Option<&str>) -> Reaction {
    Reaction {
        label: "//automation:prototype".into(),
        name: "prototype".into(),
        package: "//automation".into(),
        repo: "acme/backend".into(),
        action: action.map(str::to_owned),
        label_filter: label.map(str::to_owned),
        agent: "//tools/agent:reviewer".into(),
        prompt_template: "//tools/agent/prompts:proto".into(),
        vars: vec![],
    }
}

fn event(action: &str, labels: &[&str]) -> IssueEvent {
    IssueEvent {
        repo: "acme/backend".into(),
        action: action.into(),
        number: 7,
        title: "t".into(),
        body: "b".into(),
        url: "u".into(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        default_branch: "main".into(),
        installation_id: Some(1),
    }
}

#[test]
fn label_filter_gates_the_match() {
    let r = reaction(Some("labeled"), Some("assign-agent"));
    assert!(reaction_matches(&r, &event("labeled", &["assign-agent"])));
    assert!(!reaction_matches(&r, &event("labeled", &["other"])));
    // Wrong action.
    assert!(!reaction_matches(&r, &event("opened", &["assign-agent"])));
}

#[test]
fn default_actions_are_opened_and_labeled() {
    let r = reaction(None, None);
    assert!(reaction_matches(&r, &event("opened", &[])));
    assert!(reaction_matches(&r, &event("labeled", &[])));
    assert!(!reaction_matches(&r, &event("closed", &[])));
}

// --- end-to-end (hermetic): clone -> agent -> commit -> push -> PR ----------

/// A fake runner that creates a file in the checkout, standing in for an agent.
struct FakeRunner {
    runs: Cell<usize>,
    edit: bool,
}

impl AgentRunner for FakeRunner {
    fn run(&self, _inv: &AgentInvocation, _prompt: &str, workdir: &Path) -> Result<()> {
        self.runs.set(self.runs.get() + 1);
        if self.edit {
            std::fs::write(workdir.join("PROTOTYPE.md"), "agent prototype\n").unwrap();
        }
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a bare origin at `<base>/acme/backend` with one commit on `main`.
fn make_origin(base: &Path) -> PathBuf {
    let bare = base.join("acme/backend");
    std::fs::create_dir_all(&bare).unwrap();
    git(base, &["init", "--bare", "-b", "main", bare.to_str().unwrap()]);

    // Seed it from a throwaway working repo.
    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "backend\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(
        &seed,
        &[
            "-c",
            "user.name=Seed",
            "-c",
            "user.email=seed@example.com",
            "commit",
            "-m",
            "init",
        ],
    );
    git(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&seed, &["push", "origin", "main"]);
    bare
}

fn test_ir(root: &Path) -> Ir {
    // Prompt template referencing issue context vars, on disk under `root`.
    std::fs::write(
        root.join("proto.tmpl"),
        "Prototype issue #{{issue_number}} in {{repo}}: {{issue_title}}\n{{issue_body}}\n",
    )
    .unwrap();

    Ir {
        monorepo: Monorepo {
            name: "m".into(),
            default_branch: "main".into(),
        },
        imports: vec![],
        vendors: vec![],
        exports: vec![],
        harnesses: vec![Harness {
            label: "//tools/harness:cc".into(),
            name: "cc".into(),
            package: "//tools/harness".into(),
            kind: "claude_code".into(),
            plugins: vec![],
            skills: vec![],
        }],
        models: vec![Model {
            label: "//tools/models:opus".into(),
            name: "opus".into(),
            package: "//tools/models".into(),
            provider: "anthropic".into(),
            id: "claude-opus-4-8".into(),
            credential: None,
        }],
        agents: vec![Agent {
            label: "//tools/agent:reviewer".into(),
            name: "reviewer".into(),
            package: "//tools/agent".into(),
            harness: "//tools/harness:cc".into(),
            model: "//tools/models:opus".into(),
        }],
        prompt_templates: vec![PromptTemplate {
            label: "//tools/agent/prompts:proto".into(),
            name: "proto".into(),
            package: "//tools/agent/prompts".into(),
            src: "proto.tmpl".into(),
        }],
        reactions: vec![reaction(Some("labeled"), Some("assign-agent"))],
    }
}

#[test]
fn resolve_substitutes_issue_context_vars() {
    let root = tempfile::tempdir().unwrap();
    let ir = test_ir(root.path());
    let ev = parse_webhook_issue(&labeled_payload()).unwrap();
    let (inv, prompt) =
        resolve_reaction_invocation(&ir, &ir.reactions[0], &ev, root.path()).unwrap();
    assert_eq!(inv.provider, "anthropic");
    assert!(prompt.contains("Prototype issue #7 in acme/backend: Add a /version endpoint"));
    assert!(prompt.contains("We should expose the build version."));
    assert!(!prompt.contains("{{issue_number}}"));
}

#[test]
fn run_reaction_pushes_branch_and_records_pr() {
    let base = tempfile::tempdir().unwrap();
    let bare = make_origin(base.path());
    let root = tempfile::tempdir().unwrap();
    let ir = test_ir(root.path());
    let ev = parse_webhook_issue(&labeled_payload()).unwrap();

    let forge = LocalForge::with_base(base.path().to_path_buf());
    let runner = FakeRunner {
        runs: Cell::new(0),
        edit: true,
    };

    let outcome =
        run_reaction(&forge, &runner, &ir, &ir.reactions[0], &ev, root.path()).unwrap();

    assert_eq!(runner.runs.get(), 1);
    assert_eq!(outcome.branch.as_deref(), Some("capyfun/issue-7"));

    // The branch landed on the origin with the agent's file.
    let listed = Command::new("git")
        .args(["ls-tree", "--name-only", "capyfun/issue-7"])
        .current_dir(&bare)
        .output()
        .unwrap();
    let files = String::from_utf8_lossy(&listed.stdout);
    assert!(files.contains("PROTOTYPE.md"), "branch files: {files}");

    // A PR was recorded (LocalForge), targeting the default branch.
    let opened = forge.opened.lock().unwrap();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].0, "acme/backend");
    assert_eq!(opened[0].1.base, "main");
    assert_eq!(opened[0].1.head, "capyfun/issue-7");
}

#[test]
fn run_reaction_no_edit_opens_no_pr() {
    let base = tempfile::tempdir().unwrap();
    make_origin(base.path());
    let root = tempfile::tempdir().unwrap();
    let ir = test_ir(root.path());
    let ev = parse_webhook_issue(&labeled_payload()).unwrap();

    let forge = LocalForge::with_base(base.path().to_path_buf());
    let runner = FakeRunner {
        runs: Cell::new(0),
        edit: false,
    };

    let outcome =
        run_reaction(&forge, &runner, &ir, &ir.reactions[0], &ev, root.path()).unwrap();

    assert_eq!(outcome.branch, None);
    assert!(outcome.summary.contains("no PR"));
    assert!(forge.opened.lock().unwrap().is_empty());
}
