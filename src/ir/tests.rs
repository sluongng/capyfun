//! Tests for lowering + validation.

use super::*;
use crate::config::{
    AgentDecl, ExportDecl, GitRepoDecl, HarnessDecl, ImportDecl, ModelDecl, MonorepoDecl,
    PromptTemplateDecl,
};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn git_repo(package: &str, name: &str) -> GitRepoDecl {
    GitRepoDecl {
        name: name.into(),
        repo: "acme/plugin".into(),
        commit: SHA.into(),
        into: None,
        package: package.into(),
    }
}

fn mono(package: &str) -> Decl {
    Decl::Monorepo(MonorepoDecl {
        name: "acme".into(),
        default_branch: "main".into(),
        package: package.into(),
    })
}

fn import(package: &str, name: &str) -> ImportDecl {
    ImportDecl {
        name: name.into(),
        repo: "acme/backend".into(),
        git_ref: "refs/heads/main".into(),
        into: None,
        patches: vec![],
        package: package.into(),
    }
}

fn compile_decls(decls: Vec<Decl>) -> Result<Ir, Vec<String>> {
    compile(&RawConfig { decls })
}

fn harness(package: &str, name: &str, kind: &str) -> Decl {
    Decl::Harness(HarnessDecl {
        name: name.into(),
        kind: kind.into(),
        plugins: vec![],
        skills: vec![],
        package: package.into(),
    })
}

fn model(package: &str, name: &str, provider: &str) -> ModelDecl {
    ModelDecl {
        name: name.into(),
        provider: provider.into(),
        id: "model-id".into(),
        credential: None,
        package: package.into(),
    }
}

fn agent(package: &str, name: &str, harness: &str, model: &str) -> Decl {
    Decl::Agent(AgentDecl {
        name: name.into(),
        harness: harness.into(),
        model: model.into(),
        package: package.into(),
    })
}

#[test]
fn lowers_agent_tool_rules_and_resolves_labels() {
    let ir = compile_decls(vec![
        mono("//"),
        harness("//tools/harness", "cc", "claude_code"),
        Decl::Model(model("//tools/models", "opus", "anthropic")),
        agent(
            "//tools/agent",
            "reviewer",
            "//tools/harness:cc",
            "//tools/models:opus",
        ),
        Decl::PromptTemplate(PromptTemplateDecl {
            name: "review".into(),
            src: "review.tmpl".into(),
            package: "//tools/agent/prompts".into(),
        }),
    ])
    .unwrap();

    assert_eq!(ir.harnesses.len(), 1);
    assert_eq!(ir.harnesses[0].label, "//tools/harness:cc");
    assert_eq!(ir.models.len(), 1);
    assert_eq!(ir.models[0].label, "//tools/models:opus");

    assert_eq!(ir.agents.len(), 1);
    let a = &ir.agents[0];
    assert_eq!(a.label, "//tools/agent:reviewer");
    assert_eq!(a.harness, "//tools/harness:cc");
    assert_eq!(a.model, "//tools/models:opus");

    assert_eq!(ir.prompt_templates.len(), 1);
    assert_eq!(
        ir.prompt_templates[0].src,
        "tools/agent/prompts/review.tmpl"
    );
    assert_eq!(ir.prompt_templates[0].label, "//tools/agent/prompts:review");
}

#[test]
fn unknown_harness_kind_errors() {
    let errs = compile_decls(vec![mono("//"), harness("//t", "h", "bogus")]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("unknown harness kind")),
        "{errs:?}"
    );
}

#[test]
fn unknown_model_provider_errors() {
    let errs = compile_decls(vec![
        mono("//"),
        Decl::Model(model("//t", "m", "huggingface")),
    ])
    .unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("provider `huggingface` is unknown")),
        "{errs:?}"
    );
}

#[test]
fn empty_model_id_errors() {
    let mut m = model("//t", "m", "anthropic");
    m.id = String::new();
    let errs = compile_decls(vec![mono("//"), Decl::Model(m)]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("model id is empty")),
        "{errs:?}"
    );
}

#[test]
fn bad_credential_reference_shape_errors() {
    let mut m = model("//t", "m", "anthropic");
    m.credential = Some("vault:secret".into());
    let errs = compile_decls(vec![mono("//"), Decl::Model(m)]).unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("unrecognized credential reference")),
        "{errs:?}"
    );
}

#[test]
fn empty_credential_env_name_errors() {
    let mut m = model("//t", "m", "anthropic");
    m.credential = Some("env:".into());
    let errs = compile_decls(vec![mono("//"), Decl::Model(m)]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("empty variable name")),
        "{errs:?}"
    );
}

#[test]
fn agent_unresolved_harness_label_errors() {
    let errs = compile_decls(vec![
        mono("//"),
        Decl::Model(model("//tools/models", "opus", "anthropic")),
        agent("//t", "a", "//tools/harness:missing", "//tools/models:opus"),
    ])
    .unwrap_err();
    assert!(
        errs.iter().any(
            |e| e.contains("harness label") && e.contains("does not resolve to a harness rule")
        ),
        "{errs:?}"
    );
}

#[test]
fn agent_unresolved_model_label_errors() {
    let errs = compile_decls(vec![
        mono("//"),
        harness("//tools/harness", "cc", "claude_code"),
        agent("//t", "a", "//tools/harness:cc", "//tools/models:missing"),
    ])
    .unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("model label") && e.contains("does not resolve to a model rule")),
        "{errs:?}"
    );
}

#[test]
fn harness_cannot_drive_model_errors() {
    // claude_code paired with an OpenAI model is invalid.
    let errs = compile_decls(vec![
        mono("//"),
        harness("//tools/harness", "cc", "claude_code"),
        Decl::Model(model("//tools/models", "gpt", "openai")),
        agent(
            "//tools/agent",
            "bad",
            "//tools/harness:cc",
            "//tools/models:gpt",
        ),
    ])
    .unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("cannot drive model")),
        "{errs:?}"
    );
}

#[test]
fn prompt_template_path_escape_errors() {
    let errs = compile_decls(vec![
        mono("//"),
        Decl::PromptTemplate(PromptTemplateDecl {
            name: "p".into(),
            src: "../escape.tmpl".into(),
            package: "//t".into(),
        }),
    ])
    .unwrap_err();
    assert!(errs.iter().any(|e| e.contains("src path")), "{errs:?}");
}

#[test]
fn agent_rule_name_collides_with_model_in_same_package() {
    let errs = compile_decls(vec![
        mono("//"),
        harness("//tools/harness", "cc", "claude_code"),
        Decl::Model(model("//tools", "dup", "anthropic")),
        agent("//tools", "dup", "//tools/harness:cc", "//tools:dup"),
    ])
    .unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("duplicate rule name")),
        "{errs:?}"
    );
}

#[test]
fn lowers_paths_and_labels() {
    let mut imp = import("//third_party/backend", "backend");
    imp.patches = vec!["patches/0001.patch".into()];
    let ir = compile_decls(vec![mono("//"), Decl::Import(imp)]).unwrap();

    assert_eq!(ir.monorepo.name, "acme");
    assert_eq!(ir.imports.len(), 1);
    let i = &ir.imports[0];
    assert_eq!(i.label, "//third_party/backend:backend");
    assert_eq!(i.dest, "third_party/backend");
    assert_eq!(i.patches, vec!["third_party/backend/patches/0001.patch"]);
}

#[test]
fn into_subpath_extends_destination() {
    let mut imp = import("//third_party/backend", "backend");
    imp.into = Some("vendor/src".into());
    let ir = compile_decls(vec![mono("//"), Decl::Import(imp)]).unwrap();
    assert_eq!(ir.imports[0].dest, "third_party/backend/vendor/src");
}

#[test]
fn missing_monorepo_errors() {
    let errs = compile_decls(vec![Decl::Import(import("//svc", "a"))]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("no monorepo")), "{errs:?}");
}

#[test]
fn duplicate_monorepo_errors() {
    let errs = compile_decls(vec![mono("//"), mono("//")]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("declared 2 times")),
        "{errs:?}"
    );
}

#[test]
fn monorepo_outside_root_errors() {
    let errs = compile_decls(vec![mono("//sub")]).unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("must be declared in the root")),
        "{errs:?}"
    );
}

#[test]
fn duplicate_name_in_package_errors() {
    let errs = compile_decls(vec![
        mono("//"),
        Decl::Import(import("//svc", "dup")),
        Decl::Import(import("//svc", "dup")),
    ])
    .unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("duplicate rule name")),
        "{errs:?}"
    );
}

#[test]
fn same_name_different_package_is_ok() {
    let ir = compile_decls(vec![
        mono("//"),
        Decl::Import(import("//a", "x")),
        Decl::Import(import("//b", "x")),
    ])
    .unwrap();
    assert_eq!(ir.imports.len(), 2);
}

#[test]
fn overlapping_destinations_error() {
    let parent = import("//third_party", "p");
    let mut child = import("//third_party/backend", "c");
    child.repo = "acme/child".into();
    let errs =
        compile_decls(vec![mono("//"), Decl::Import(parent), Decl::Import(child)]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("destination overlap")),
        "{errs:?}"
    );
}

#[test]
fn bad_slug_errors() {
    let mut imp = import("//svc", "a");
    imp.repo = "not-a-slug".into();
    let errs = compile_decls(vec![mono("//"), Decl::Import(imp)]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("owner/name")), "{errs:?}");
}

#[test]
fn into_escape_errors() {
    let mut imp = import("//svc", "a");
    imp.into = Some("../escape".into());
    let errs = compile_decls(vec![mono("//"), Decl::Import(imp)]).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("into path")), "{errs:?}");
}

#[test]
fn export_lowers_source_path() {
    let exp = ExportDecl {
        name: "sdk".into(),
        from_path: Some("go".into()),
        repo: "acme/sdk-go".into(),
        branch: "main".into(),
        package: "//sdk".into(),
    };
    let ir = compile_decls(vec![mono("//"), Decl::Export(exp)]).unwrap();
    assert_eq!(ir.exports[0].from, "sdk/go");
    assert_eq!(ir.exports[0].label, "//sdk:sdk");
}

#[test]
fn git_repository_lowers_to_vendor() {
    let ir = compile_decls(vec![
        mono("//"),
        Decl::GitRepo(git_repo("//tools/cc", "cc")),
    ])
    .unwrap();
    assert_eq!(ir.vendors.len(), 1);
    let v = &ir.vendors[0];
    assert_eq!(v.label, "//tools/cc:cc");
    assert_eq!(v.repo, "acme/plugin");
    assert_eq!(v.commit, SHA);
    assert_eq!(v.dest, "tools/cc");
}

#[test]
fn bad_commit_sha_errors() {
    let mut g = git_repo("//tools/cc", "cc");
    g.commit = "deadbeef".into();
    let errs = compile_decls(vec![mono("//"), Decl::GitRepo(g)]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("40-character hex SHA")),
        "{errs:?}"
    );
}

#[test]
fn vendor_and_import_destinations_must_not_overlap() {
    // An import at //third_party and a git_repository nested under it conflict.
    let imp = import("//third_party", "p");
    let g = git_repo("//third_party/cc", "cc");
    let errs = compile_decls(vec![mono("//"), Decl::Import(imp), Decl::GitRepo(g)]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("destination overlap")),
        "{errs:?}"
    );
}

#[test]
fn vendor_name_collides_with_import_in_same_package() {
    let imp = import("//tools/x", "dup");
    let g = git_repo("//tools/x", "dup");
    let errs = compile_decls(vec![mono("//"), Decl::Import(imp), Decl::GitRepo(g)]).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("duplicate rule name")),
        "{errs:?}"
    );
}

#[test]
fn errors_are_sorted_and_deduped() {
    // Two distinct problems; output must be deterministic.
    let mut imp = import("//svc", "a");
    imp.repo = "bad".into();
    imp.into = Some("/abs".into());
    let errs = compile_decls(vec![Decl::Import(imp)]).unwrap_err();
    let mut sorted = errs.clone();
    sorted.sort();
    assert_eq!(errs, sorted);
}
