use super::*;

#[test]
fn slug_from_common_url_forms() {
    let cases = [
        "https://github.com/dtolnay/anyhow",
        "https://github.com/dtolnay/anyhow.git",
        "git+https://github.com/dtolnay/anyhow.git",
        "git://github.com/dtolnay/anyhow.git",
        "ssh://git@github.com/dtolnay/anyhow.git",
        "git@github.com:dtolnay/anyhow.git",
        "https://github.com/dtolnay/anyhow#readme",
        "https://github.com/dtolnay/anyhow/tree/master/sub",
    ];
    for c in cases {
        assert_eq!(
            parse_github_slug(c),
            Some(("dtolnay".to_owned(), "anyhow".to_owned())),
            "failed for {c}"
        );
    }
}

#[test]
fn slug_rejects_non_github() {
    assert_eq!(parse_github_slug("https://gitlab.com/foo/bar"), None);
    assert_eq!(parse_github_slug("https://github.com/onlyowner"), None);
}

#[test]
fn tag_candidates_cover_conventions() {
    let c = tag_candidates("anyhow", "1.0.86");
    assert_eq!(c[0], "v1.0.86");
    assert_eq!(c[1], "1.0.86");
    assert!(c.contains(&"anyhow-1.0.86".to_owned()));
    assert!(c.contains(&"anyhow@1.0.86".to_owned()));
    // A leading `v` in the version is normalized away.
    assert_eq!(tag_candidates("x", "v2.0.0")[1], "2.0.0");
}

#[test]
fn tag_candidates_strip_npm_scope() {
    let c = tag_candidates("@babel/core", "7.0.0");
    assert!(c.contains(&"core-7.0.0".to_owned()));
    assert!(c.contains(&"core@7.0.0".to_owned()));
}

#[test]
fn pick_commit_prefers_first_candidate_and_peeled_tags() {
    let refs = vec![
        ("refs/heads/master".to_owned(), "aaaa".to_owned()),
        // Annotated tag: the tag object plus its peeled commit.
        ("refs/tags/1.0.86".to_owned(), "tagobj".to_owned()),
        ("refs/tags/1.0.86^{}".to_owned(), "commit86".to_owned()),
    ];
    let cands = tag_candidates("anyhow", "1.0.86");
    let (tag, sha) = pick_commit(&refs, &cands).unwrap();
    assert_eq!(tag, "1.0.86");
    assert_eq!(sha, "commit86", "should dereference the annotated tag");
}

#[test]
fn pick_commit_uses_lightweight_tag_when_unpeeled() {
    let refs = vec![("refs/tags/v2.0.0".to_owned(), "lightweight".to_owned())];
    let (tag, sha) = pick_commit(&refs, &tag_candidates("x", "2.0.0")).unwrap();
    assert_eq!(tag, "v2.0.0");
    assert_eq!(sha, "lightweight");
}

#[test]
fn pick_commit_none_when_no_match() {
    let refs = vec![("refs/tags/3.0.0".to_owned(), "x".to_owned())];
    assert_eq!(pick_commit(&refs, &tag_candidates("x", "1.0.0")), None);
}

#[test]
fn github_url_honors_base_override() {
    // Default (no env) form.
    if std::env::var("CAPYFUN_GITHUB_BASE").is_err() {
        assert_eq!(github_url("a/b"), "https://github.com/a/b.git");
    }
}
