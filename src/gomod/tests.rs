use super::*;

const GO_MOD: &str = r#"
module github.com/acme/demo

go 1.22

require (
	github.com/google/uuid v1.6.0
	github.com/pkg/errors v0.9.1
	github.com/spf13/pflag v1.0.5 // indirect
	golang.org/x/sys v0.18.0 // indirect
	github.com/exp/pseudo v0.0.0-20230101000000-abcdefabcdef
)

require github.com/spf13/cobra v1.8.0
"#;

#[test]
fn parses_block_and_single_requires() {
    let reqs = parse_go_mod(GO_MOD);
    assert_eq!(reqs.len(), 6);
    let uuid = reqs
        .iter()
        .find(|r| r.path == "github.com/google/uuid")
        .unwrap();
    assert_eq!(uuid.version, "v1.6.0");
    assert!(!uuid.indirect);
    let pflag = reqs
        .iter()
        .find(|r| r.path == "github.com/spf13/pflag")
        .unwrap();
    assert!(pflag.indirect);
    let cobra = reqs
        .iter()
        .find(|r| r.path == "github.com/spf13/cobra")
        .unwrap();
    assert_eq!(cobra.version, "v1.8.0");
}

#[test]
fn plans_direct_github_imports_only_by_default() {
    let reqs = parse_go_mod(GO_MOD);
    let (imports, skipped) = plan_imports(&reqs, "third_party", false);

    // Direct github modules: uuid, errors, cobra. pflag is indirect (skipped),
    // x/sys is not github, example.com/foo/bar is a pseudo-version.
    let dirs: Vec<&str> = imports.iter().map(|i| i.package_dir.as_str()).collect();
    assert_eq!(
        dirs,
        vec![
            "third_party/github.com/google/uuid",
            "third_party/github.com/pkg/errors",
            "third_party/github.com/spf13/cobra",
        ]
    );
    let uuid = &imports[0];
    assert_eq!(uuid.name, "uuid");
    assert_eq!(uuid.slug, "google/uuid");
    assert_eq!(uuid.git_ref, "refs/tags/v1.6.0");

    // Pseudo-version is reported as skipped.
    assert!(skipped.iter().any(|s| s.contains("pseudo-version")));
}

#[test]
fn includes_indirect_when_requested() {
    let reqs = parse_go_mod(GO_MOD);
    let (imports, _) = plan_imports(&reqs, "third_party", true);
    assert!(imports.iter().any(|i| i.slug == "spf13/pflag"));
}

#[test]
fn strips_major_version_and_incompatible() {
    let reqs = vec![
        Require {
            path: "github.com/foo/bar/v2".into(),
            version: "v2.3.4".into(),
            indirect: false,
        },
        Require {
            path: "github.com/baz/qux".into(),
            version: "v3.0.0+incompatible".into(),
            indirect: false,
        },
    ];
    let (imports, _) = plan_imports(&reqs, "vendor", false);
    let bar = imports.iter().find(|i| i.slug == "foo/bar").unwrap();
    assert_eq!(bar.package_dir, "vendor/github.com/foo/bar");
    assert_eq!(bar.git_ref, "refs/tags/v2.3.4");
    let qux = imports.iter().find(|i| i.slug == "baz/qux").unwrap();
    assert_eq!(qux.git_ref, "refs/tags/v3.0.0");
}

#[test]
fn dedups_same_repo() {
    let reqs = vec![
        Require {
            path: "github.com/foo/bar".into(),
            version: "v1.0.0".into(),
            indirect: false,
        },
        Require {
            path: "github.com/foo/bar/subpkg".into(),
            version: "v1.0.0".into(),
            indirect: false,
        },
    ];
    let (imports, _) = plan_imports(&reqs, "third_party", false);
    assert_eq!(imports.len(), 1);
}

#[test]
fn pseudo_version_detection() {
    assert!(is_pseudo_version("v0.0.0-20230101000000-abcdefabcdef"));
    assert!(is_pseudo_version("v1.2.3-0.20230101000000-0123456789ab"));
    assert!(!is_pseudo_version("v1.8.0"));
    assert!(!is_pseudo_version("v2.0.0+incompatible"));
}

#[test]
fn go_sum_pairs() {
    let sum =
        "github.com/google/uuid v1.6.0 h1:AAA=\ngithub.com/google/uuid v1.6.0/go.mod h1:BBB=\n";
    let set = parse_go_sum(sum);
    assert!(set.contains(&("github.com/google/uuid".into(), "v1.6.0".into())));
    assert_eq!(set.len(), 1, "the /go.mod line collapses to the same pair");
}

#[test]
fn rendered_src_is_valid_shape() {
    let gi = GithubImport {
        module_path: "github.com/spf13/cobra".into(),
        package_dir: "third_party/github.com/spf13/cobra".into(),
        name: "cobra".into(),
        slug: "spf13/cobra".into(),
        git_ref: "refs/tags/v1.8.0".into(),
    };
    let src = render_src(&gi);
    assert!(src.contains("github_import("));
    assert!(src.contains("name = \"cobra\""));
    assert!(src.contains("repo = \"spf13/cobra\""));
    assert!(src.contains("ref = \"refs/tags/v1.8.0\""));
}
