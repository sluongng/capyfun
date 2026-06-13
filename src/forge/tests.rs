use super::*;
use std::path::PathBuf;

#[test]
fn app_forge_rejects_a_bad_private_key() {
    // Map the Ok value away: `GitHubAppForge` holds a non-Debug `EncodingKey`.
    let err = GitHubAppForge::new("123", b"not a pem", "https://api.github.com")
        .map(|_| ())
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("private key"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn local_forge_git_url_joins_base_and_slug() {
    let forge = LocalForge::with_base(PathBuf::from("/tmp/capy-origins"));
    let url = forge.git_url("acme/backend", None).unwrap();
    assert_eq!(url, "/tmp/capy-origins/acme/backend");
}

#[test]
fn local_forge_records_prs_instead_of_opening() {
    let forge = LocalForge::new();
    let pr = PrRequest {
        base: "main".into(),
        head: "capyfun/issue-7".into(),
        title: "Prototype issue #7".into(),
        body: "Closes #7".into(),
    };
    let out = forge.open_pr("acme/backend", None, &pr).unwrap();
    assert!(out.url.is_none());
    assert!(out.summary.contains("recorded PR on acme/backend"));

    let opened = forge.opened.lock().unwrap();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].0, "acme/backend");
    assert_eq!(opened[0].1, pr);
}
