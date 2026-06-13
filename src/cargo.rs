//! Minimal `Cargo.toml` / `Cargo.lock` reader for scaffolding `git_repository`
//! snapshot rules (fine-grained: one `SRC` per crate, mirroring `gen-go`).
//!
//! Parses `[dependencies]` from the manifest and `[[package]]` blocks from the
//! lock. Registry crates carry no upstream repo, so their GitHub slug + commit
//! are resolved over the network in the CLI layer; git-source crates
//! (`{ git = "..." }`) carry the repo (and, via the lock, the exact SHA) inline
//! and resolve purely. This reader is pure: no Cargo toolchain or network.
//!
//! It is a deliberately small TOML subset, not a full parser: section headers,
//! `key = "string"`, and single-line inline tables (`key = { version = ".." }`).
//! Multi-line inline tables are not parsed.

use std::collections::BTreeMap;

use crate::vendorgen::{parse_github_slug, Vendored};

/// A `[dependencies]` entry from a `Cargo.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub name: String,
    /// Version requirement string (e.g. `1`, `1.0.86`), empty for pure git deps.
    pub req: String,
    /// A `{ git = "..." }` source URL, if this is a git dependency.
    pub git: Option<String>,
}

/// A resolved `[[package]]` entry from a `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    pub version: String,
    /// The lock's `source` field, e.g. `git+https://github.com/x/y?rev=..#<sha>`.
    pub source: Option<String>,
}

/// A planned crate snapshot before network resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub name: String,
    /// Monorepo-relative package directory, e.g. `third_party/rust/anyhow`.
    pub package_dir: String,
    /// Exact version (from the lock) or the manifest requirement otherwise.
    pub version: String,
    /// GitHub repo URL for a git dependency (skips the registry lookup).
    pub git_url: Option<String>,
    /// Exact commit SHA from a git-source lock entry (skips `ls-remote`).
    pub git_sha: Option<String>,
}

/// Parse `[dependencies]` of a `Cargo.toml`.
pub fn parse_manifest(content: &str) -> Vec<Dep> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for raw in content.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_deps = header.trim() == "dependencies";
            continue;
        }
        if !in_deps {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let name = key.trim().trim_matches('"').to_owned();
        let val = val.trim();
        if let Some(table) = val.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            out.push(Dep {
                name,
                req: inline_value(table, "version").unwrap_or_default(),
                git: inline_value(table, "git"),
            });
        } else {
            out.push(Dep {
                name,
                req: val.trim_matches('"').to_owned(),
                git: None,
            });
        }
    }
    out
}

/// Parse `[[package]]` blocks of a `Cargo.lock` into `name -> entry`.
pub fn parse_lock(content: &str) -> BTreeMap<String, LockEntry> {
    let mut out = BTreeMap::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut source: Option<String> = None;
    let flush = |name: &mut Option<String>,
                 version: &mut Option<String>,
                 source: &mut Option<String>,
                 out: &mut BTreeMap<String, LockEntry>| {
        if let (Some(n), Some(v)) = (name.take(), version.take()) {
            out.insert(
                n,
                LockEntry {
                    version: v,
                    source: source.take(),
                },
            );
        }
        *source = None;
    };
    for raw in content.lines() {
        let line = strip_comment(raw).trim();
        if line == "[[package]]" {
            flush(&mut name, &mut version, &mut source, &mut out);
            continue;
        }
        if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim().trim_matches('"').to_owned());
        } else if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim().trim_matches('"').to_owned());
        } else if let Some(v) = line.strip_prefix("source = ") {
            source = Some(v.trim().trim_matches('"').to_owned());
        }
    }
    flush(&mut name, &mut version, &mut source, &mut out);
    out
}

/// Plan crate snapshots: resolve each manifest dep's exact version from the lock,
/// thread through any git source, and assign a fine-grained package directory.
/// Deduplicated by name, sorted by package directory for deterministic output.
pub fn plan(deps: &[Dep], lock: &BTreeMap<String, LockEntry>, prefix: &str) -> Vec<Planned> {
    let mut planned: Vec<Planned> = Vec::new();
    for dep in deps {
        let entry = lock.get(&dep.name);
        let version = entry
            .map(|e| e.version.clone())
            .unwrap_or_else(|| dep.req.clone());
        // A git source can come from the manifest (`{ git = .. }`) or the lock's
        // resolved `source = "git+..#<sha>"`.
        let lock_git = entry
            .and_then(|e| e.source.as_deref())
            .filter(|s| s.starts_with("git+"));
        let git_url = dep
            .git
            .clone()
            .or_else(|| lock_git.map(|s| s.trim_start_matches("git+").to_owned()));
        let git_sha = lock_git.and_then(|s| s.rsplit_once('#').map(|(_, sha)| sha.to_owned()));
        let p = Planned {
            name: dep.name.clone(),
            package_dir: format!("{prefix}/{}", dep.name),
            version,
            git_url,
            git_sha,
        };
        if !planned.iter().any(|e| e.name == p.name) {
            planned.push(p);
        }
    }
    planned.sort_by(|a, b| a.package_dir.cmp(&b.package_dir));
    planned
}

/// Render the `SRC` body for a resolved crate snapshot.
pub fn render_src(v: &Vendored) -> String {
    format!(
        "# Generated by `capyfun gen-cargo` from Cargo.toml ({} {}).\ngit_repository(\n    name = {:?},\n    repo = {:?},\n    commit = {:?},\n)\n",
        v.name, v.version, v.name, v.slug, v.commit
    )
}

/// Resolve a planned crate's GitHub slug from its git source, if it has one.
pub fn slug_from_git(p: &Planned) -> Option<(String, String)> {
    p.git_url.as_deref().and_then(parse_github_slug)
}

/// Strip a trailing `# comment`, but only when the `#` is outside a quoted
/// string — a git `source = "git+..#<sha>"` carries a `#` inside its value.
fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            '#' if !in_quote => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Extract `key = "value"` from a single-line inline table body.
fn inline_value(table: &str, key: &str) -> Option<String> {
    for part in table.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let v = rest.trim().trim_matches('"').to_owned();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
