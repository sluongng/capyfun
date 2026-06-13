//! Minimal `go.mod` / `go.sum` reader for scaffolding CapyFun import rules.
//!
//! Parses the `require` directives of a `go.mod` and maps GitHub-hosted modules
//! to `github_import` rules (one package per `github.com/<owner>/<repo>`, tracked
//! at the pinned version's tag). Pure: no Go toolchain or network involved.

use std::collections::BTreeSet;

/// A single `require` entry from a `go.mod`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Require {
    pub path: String,
    pub version: String,
    pub indirect: bool,
}

/// A GitHub module mapped to a CapyFun import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubImport {
    /// Original module path, e.g. `github.com/spf13/cobra`.
    pub module_path: String,
    /// Monorepo-relative package directory, e.g. `third_party/github.com/spf13/cobra`.
    pub package_dir: String,
    /// Rule name (the repository name), e.g. `cobra`.
    pub name: String,
    /// GitHub `owner/name` slug.
    pub slug: String,
    /// Tracked ref, e.g. `refs/tags/v1.8.0`.
    pub git_ref: String,
}

/// Parse the `require` directives of a `go.mod`.
pub fn parse_go_mod(content: &str) -> Vec<Require> {
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in content.lines() {
        let trimmed = raw.trim();
        if in_block {
            if trimmed == ")" {
                in_block = false;
            } else if !trimmed.is_empty() {
                if let Some(req) = parse_require_line(trimmed) {
                    out.push(req);
                }
            }
        } else if trimmed == "require (" {
            in_block = true;
        } else if let Some(rest) = trimmed.strip_prefix("require ") {
            if let Some(req) = parse_require_line(rest.trim()) {
                out.push(req);
            }
        }
    }
    out
}

fn parse_require_line(line: &str) -> Option<Require> {
    let (code, comment) = match line.split_once("//") {
        Some((c, cm)) => (c.trim(), cm.trim()),
        None => (line, ""),
    };
    let mut it = code.split_whitespace();
    let path = it.next()?.to_owned();
    let version = it.next()?.to_owned();
    Some(Require {
        path,
        version,
        indirect: comment == "indirect",
    })
}

/// Parse `go.sum` into the set of `(module, version)` pairs it records (the
/// `/go.mod` suffix is stripped). Used to cross-check `go.mod` selections.
pub fn parse_go_sum(content: &str) -> BTreeSet<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let path = it.next()?;
            let version = it.next()?;
            let version = version.strip_suffix("/go.mod").unwrap_or(version);
            Some((path.to_owned(), version.to_owned()))
        })
        .collect()
}

/// A Go pseudo-version (`vX.Y.Z-yyyymmddhhmmss-abcdefabcdef`) has no tag, so it
/// cannot be mapped to `refs/tags/...`.
fn is_pseudo_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('-').collect();
    if parts.len() < 2 {
        return false;
    }
    let hash = parts[parts.len() - 1];
    let ts = parts[parts.len() - 2];
    // The timestamp may be prefixed (e.g. `pre.0.<ts>`); match its trailing run.
    let ts_ok = ts.len() >= 14
        && ts.as_bytes()[ts.len() - 14..]
            .iter()
            .all(u8::is_ascii_digit);
    hash.len() == 12 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && ts_ok
}

/// Map a module version to a Git tag ref, or `None` for pseudo-versions.
fn version_to_ref(version: &str) -> Option<String> {
    if is_pseudo_version(version) {
        return None;
    }
    let v = version.strip_suffix("+incompatible").unwrap_or(version);
    Some(format!("refs/tags/{v}"))
}

/// Map a `require` to a [`GithubImport`], or `None` if it is not a tag-pinned
/// `github.com` module. `prefix` is the third-party root (e.g. `third_party`).
pub fn to_github_import(req: &Require, prefix: &str) -> Option<GithubImport> {
    let segs: Vec<&str> = req.path.split('/').collect();
    if segs.first() != Some(&"github.com") || segs.len() < 3 {
        return None;
    }
    let (owner, repo) = (segs[1], segs[2]);
    let git_ref = version_to_ref(&req.version)?;
    Some(GithubImport {
        module_path: req.path.clone(),
        package_dir: format!("{prefix}/github.com/{owner}/{repo}"),
        name: repo.to_owned(),
        slug: format!("{owner}/{repo}"),
        git_ref,
    })
}

/// Map a set of requires to deduplicated GitHub imports (one per repo), sorted
/// by package directory for deterministic output. Non-GitHub and pseudo-version
/// modules are dropped (returned separately as skip reasons).
pub fn plan_imports(
    requires: &[Require],
    prefix: &str,
    include_indirect: bool,
) -> (Vec<GithubImport>, Vec<String>) {
    let mut imports: Vec<GithubImport> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for req in requires {
        if req.indirect && !include_indirect {
            continue;
        }
        match to_github_import(req, prefix) {
            Some(gi) => {
                if !imports.iter().any(|e| e.package_dir == gi.package_dir) {
                    imports.push(gi);
                }
            }
            None => {
                let reason = if req.path.starts_with("github.com/") {
                    "pseudo-version (no tag)"
                } else {
                    "not a github.com module"
                };
                skipped.push(format!("{} {} ({reason})", req.path, req.version));
            }
        }
    }
    imports.sort_by(|a, b| a.package_dir.cmp(&b.package_dir));
    (imports, skipped)
}

/// Render the `SRC` file body for a generated import.
pub fn render_src(gi: &GithubImport) -> String {
    format!(
        "# Generated by `capyfun gen-go` from go.mod ({}).\ngithub_import(\n    name = {:?},\n    repo = {:?},\n    ref = {:?},\n)\n",
        gi.module_path, gi.name, gi.slug, gi.git_ref
    )
}

#[cfg(test)]
mod tests;
