//! Tests for lowering + validation.

use super::*;
use crate::config::{ExportDecl, ImportDecl, MonorepoDecl};

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
