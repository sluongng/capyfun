//! Shared helpers for the snapshot generators (`gen-cargo`, `gen-npm`).
//!
//! Unlike `go.mod` (whose module path *is* a GitHub slug and whose version *is*
//! a tag), Cargo and npm name packages by a registry identity that does not
//! encode the upstream GitHub repository. Mapping `anyhow` -> `dtolnay/anyhow`
//! -> a commit SHA therefore needs two lookups: the registry's recorded
//! repository URL, and the version's tag resolved against the remote.
//!
//! The pure helpers ([`parse_github_slug`], [`tag_candidates`], [`pick_commit`])
//! are unit-tested; the registry/`ls-remote` functions do network I/O and are
//! exercised by the demo rather than hermetic tests.

use anyhow::{Context, Result};

/// A package resolved to a pinned GitHub snapshot, ready to render as a
/// `git_repository` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vendored {
    /// Rule name (the package name), e.g. `anyhow`.
    pub name: String,
    /// GitHub `owner/repo` slug, e.g. `dtolnay/anyhow`.
    pub slug: String,
    /// The package version this snapshot corresponds to, e.g. `1.0.86`.
    pub version: String,
    /// The resolved 40-char commit SHA to pin.
    pub commit: String,
    /// The tag the commit was resolved from, e.g. `1.0.86` (for reporting).
    pub tag: String,
}

/// Resolve a GitHub base URL for a `owner/repo` slug.
///
/// `CAPYFUN_GITHUB_BASE` overrides the GitHub base (used to point generators and
/// imports at local bare repositories in hermetic demos); otherwise the public
/// GitHub HTTPS URL is used.
pub fn github_url(slug: &str) -> String {
    match std::env::var("CAPYFUN_GITHUB_BASE") {
        Ok(base) => format!("{}/{}", base.trim_end_matches('/'), slug),
        Err(_) => format!("https://github.com/{slug}.git"),
    }
}

/// Parse a GitHub `owner/repo` from a repository URL as recorded by a package
/// registry. Handles `https://`, `git+https://`, `git://`, `ssh://`, and
/// `git@github.com:` forms, an optional `.git` suffix, `#`/`?` fragments, and
/// trailing subpaths (a monorepo `.../tree/main/sub` collapses to `owner/repo`).
/// Non-GitHub hosts return `None`.
pub fn parse_github_slug(url: &str) -> Option<(String, String)> {
    let idx = url.find("github.com")?;
    let after = &url[idx + "github.com".len()..];
    // Strip the host/path separator (`/` for URLs, `:` for scp-like syntax).
    let after = after.trim_start_matches(['/', ':']);
    // Drop any `#fragment` or `?query`.
    let after = after.split(['#', '?']).next().unwrap_or(after);
    let mut segs = after.split('/').filter(|s| !s.is_empty());
    let owner = segs.next()?.to_owned();
    let repo = segs.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo).to_owned();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

/// Ordered tag-name candidates for a package version, most likely first. Covers
/// the common conventions: bare `v1.2.3` / `1.2.3`, and the monorepo styles
/// `<pkg>-v1.2.3`, `<pkg>-1.2.3`, `<pkg>@1.2.3`. The package's registry scope
/// (an npm `@scope/`) is stripped for the package-prefixed forms.
pub fn tag_candidates(name: &str, version: &str) -> Vec<String> {
    let v = version.trim().trim_start_matches('v');
    let bare = name.rsplit('/').next().unwrap_or(name);
    let mut c = vec![
        format!("v{v}"),
        v.to_owned(),
        format!("{bare}-v{v}"),
        format!("{bare}-{v}"),
        format!("{bare}@{v}"),
    ];
    c.dedup();
    c
}

/// Given the `(ref, sha)` advertisements from `ls-remote` and ordered tag
/// candidates, pick the commit for the first candidate that resolves. Annotated
/// tags are dereferenced via their peeled `^{}` entry so the result is always a
/// commit, never a tag object. Returns `(tag, sha)`.
pub fn pick_commit(refs: &[(String, String)], candidates: &[String]) -> Option<(String, String)> {
    for cand in candidates {
        let tag = format!("refs/tags/{cand}");
        let peeled = format!("{tag}^{{}}");
        if let Some((_, sha)) = refs.iter().find(|(n, _)| n == &peeled) {
            return Some((cand.clone(), sha.clone()));
        }
        if let Some((_, sha)) = refs.iter().find(|(n, _)| n == &tag) {
            return Some((cand.clone(), sha.clone()));
        }
    }
    None
}

// --- network resolution (not hermetically unit-tested) ---

/// List a GitHub repo's refs (`name`, hex `sha`) via an anonymous `ls-remote`.
pub fn ls_remote(slug: &str) -> Result<Vec<(String, String)>> {
    let url = github_url(slug);
    let mut remote = git2::Remote::create_detached(url.as_str())
        .with_context(|| format!("creating remote for {slug}"))?;
    remote
        .connect(git2::Direction::Fetch)
        .with_context(|| format!("connecting to {url}"))?;
    let heads = remote
        .list()
        .with_context(|| format!("listing refs of {url}"))?;
    let out = heads
        .iter()
        .map(|h| (h.name().to_owned(), h.oid().to_string()))
        .collect();
    remote.disconnect().ok();
    Ok(out)
}

/// Fetch the repository URL crates.io records for a crate, if any.
pub fn crates_io_repo(name: &str) -> Result<Option<String>> {
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let body = http_get_json(&url)?;
    Ok(body
        .get("crate")
        .and_then(|c| c.get("repository"))
        .and_then(|r| r.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned))
}

/// Fetch the repository URL the npm registry records for a package, if any.
/// `repository` may be a bare string or a `{ "type", "url" }` object.
pub fn npm_repo(name: &str) -> Result<Option<String>> {
    let url = format!("https://registry.npmjs.org/{name}");
    let body = http_get_json(&url)?;
    let repo = match body.get("repository") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Object(o)) => {
            o.get("url").and_then(|u| u.as_str()).map(str::to_owned)
        }
        _ => None,
    };
    Ok(repo.filter(|s| !s.is_empty()))
}

fn http_get_json(url: &str) -> Result<serde_json::Value> {
    // crates.io rejects requests without a descriptive User-Agent.
    let resp = ureq::get(url)
        .header(
            "User-Agent",
            "capyfun-gen (https://github.com/tinytree/capyfun)",
        )
        .call()
        .with_context(|| format!("GET {url}"))?;
    let body = resp
        .into_body()
        .read_to_string()
        .with_context(|| format!("reading response body of {url}"))?;
    serde_json::from_str(&body).with_context(|| format!("parsing JSON from {url}"))
}

#[cfg(test)]
mod tests;
