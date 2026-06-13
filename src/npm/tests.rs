use super::*;

const MANIFEST: &str = r#"{
  "name": "demo",
  "version": "0.1.0",
  "dependencies": {
    "ansi-styles": "^6.2.1",
    "escape-string-regexp": "^5.0.0"
  },
  "devDependencies": {
    "typescript": "^5"
  }
}"#;

const LOCK_V3: &str = r#"{
  "name": "demo",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "demo", "version": "0.1.0" },
    "node_modules/ansi-styles": { "version": "6.2.1" },
    "node_modules/escape-string-regexp": { "version": "5.0.0" },
    "node_modules/ansi-styles/node_modules/color-convert": { "version": "2.0.1" }
  }
}"#;

#[test]
fn parses_manifest_dependencies_only() {
    let deps = parse_manifest(MANIFEST).unwrap();
    let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["ansi-styles", "escape-string-regexp"]);
    assert_eq!(deps[0].range, "^6.2.1");
}

#[test]
fn parses_lock_top_level_only() {
    let lock = parse_lock(LOCK_V3).unwrap();
    assert_eq!(lock["ansi-styles"], "6.2.1");
    assert_eq!(lock["escape-string-regexp"], "5.0.0");
    assert!(
        !lock.contains_key("color-convert"),
        "nested transitive install is skipped"
    );
}

#[test]
fn plan_uses_lock_version_and_into() {
    let deps = parse_manifest(MANIFEST).unwrap();
    let lock = parse_lock(LOCK_V3).unwrap();
    let planned = plan(&deps, &lock);
    let ansi = &planned[0];
    assert_eq!(ansi.name, "ansi-styles");
    assert_eq!(ansi.version, "6.2.1");
    assert_eq!(ansi.into, "ansi-styles");
}

#[test]
fn plan_strips_range_without_lock() {
    let deps = parse_manifest(MANIFEST).unwrap();
    let planned = plan(&deps, &BTreeMap::new());
    assert_eq!(planned[0].version, "6.2.1", "operator stripped from ^6.2.1");
}

#[test]
fn render_src_emits_targets_with_into() {
    let v = Vendored {
        name: "ansi-styles".into(),
        slug: "chalk/ansi-styles".into(),
        version: "6.2.1".into(),
        commit: "faf414e7b479435b5a86b15ecb13fe89ecf5bd0e".into(),
        tag: "v6.2.1".into(),
    };
    let src = render_src(&[(v, "ansi-styles".to_owned())]);
    assert!(src.contains("git_repository("));
    assert!(src.contains("repo = \"chalk/ansi-styles\""));
    assert!(src.contains("into = \"ansi-styles\""));
}
