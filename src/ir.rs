//! Normalized CapyFun IR.
//!
//! Lowers captured config [`Decl`](crate::config::Decl)s into a deterministic,
//! validated intermediate representation: labels resolved, destination/source
//! paths package-anchored (monorepo-root-relative), and statically checked
//! before any Git operation runs.

use serde::Serialize;

use crate::config::{Decl, RawConfig};
use crate::validate;

/// The validated, normalized configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ir {
    pub monorepo: Monorepo,
    pub imports: Vec<Import>,
    pub vendors: Vec<Vendor>,
    pub exports: Vec<Export>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Monorepo {
    pub name: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Import {
    /// Bazel-style label, e.g. `//third_party/backend:backend`.
    pub label: String,
    pub name: String,
    pub package: String,
    /// GitHub `owner/name` slug.
    pub repo: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Monorepo-root-relative destination directory the import lands in.
    pub dest: String,
    /// Monorepo-root-relative patch files (resolved against the SRC package).
    pub patches: Vec<String>,
}

/// A pinned-snapshot git dependency (`git_repository`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Vendor {
    pub label: String,
    pub name: String,
    pub package: String,
    /// GitHub `owner/name` slug.
    pub repo: String,
    /// Exact commit SHA (the pin).
    pub commit: String,
    /// Monorepo-root-relative destination directory the snapshot lands in.
    pub dest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Export {
    pub label: String,
    pub name: String,
    pub package: String,
    pub repo: String,
    pub branch: String,
    /// Monorepo-root-relative source directory to export.
    pub from: String,
}

/// The monorepo-root-relative directory for a package label (`//` -> "").
fn package_dir(package: &str) -> &str {
    package.strip_prefix("//").unwrap_or(package)
}

/// Join a package directory with an optional subpath, keeping forward slashes.
fn join_dir(dir: &str, sub: Option<&str>) -> String {
    match sub {
        Some(sub) if dir.is_empty() => sub.to_owned(),
        Some(sub) => format!("{dir}/{sub}"),
        None => dir.to_owned(),
    }
}

/// Lower and validate a [`RawConfig`] into [`Ir`].
///
/// Returns all diagnostics (sorted) on failure rather than stopping at the
/// first, so a single run surfaces every problem.
pub fn compile(raw: &RawConfig) -> Result<Ir, Vec<String>> {
    let mut errors = Vec::new();

    // --- monorepo singleton, in the root package ---
    let monorepos: Vec<_> = raw
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Monorepo(m) => Some(m),
            _ => None,
        })
        .collect();
    let monorepo = match monorepos.as_slice() {
        [] => {
            errors.push("no monorepo(...) declared; the root SRC must declare exactly one".into());
            None
        }
        [m] => {
            if m.package != "//" {
                errors.push(format!(
                    "monorepo(...) must be declared in the root SRC (//), not in {}",
                    m.package
                ));
            }
            Some(Monorepo {
                name: m.name.clone(),
                default_branch: m.default_branch.clone(),
            })
        }
        many => {
            let pkgs: Vec<&str> = many.iter().map(|m| m.package.as_str()).collect();
            errors.push(format!(
                "monorepo(...) declared {} times (in {}); exactly one is allowed",
                many.len(),
                pkgs.join(", ")
            ));
            None
        }
    };

    // --- lower imports/vendors/exports ---
    let mut imports = Vec::new();
    let mut vendors = Vec::new();
    let mut exports = Vec::new();
    for decl in &raw.decls {
        match decl {
            Decl::Monorepo(_) => {}
            Decl::GitRepo(d) => {
                let label = format!("{}:{}", d.package, d.name);
                validate::check_slug(&label, &d.repo, &mut errors);
                validate::check_commit_sha(&label, &d.commit, &mut errors);
                if let Some(into) = &d.into {
                    validate::check_rel_path(&label, "into", into, &mut errors);
                }
                let dir = package_dir(&d.package);
                let dest = join_dir(dir, d.into.as_deref());
                if dest.is_empty() {
                    errors.push(format!(
                        "{label}: git_repository targets the monorepo root; declare it in a sub-package or set `into`"
                    ));
                }
                vendors.push(Vendor {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    repo: d.repo.clone(),
                    commit: d.commit.clone(),
                    dest,
                });
            }
            Decl::Import(d) => {
                let label = format!("{}:{}", d.package, d.name);
                validate::check_slug(&label, &d.repo, &mut errors);
                if d.git_ref.is_empty() {
                    errors.push(format!("{label}: ref is empty"));
                }
                if let Some(into) = &d.into {
                    validate::check_rel_path(&label, "into", into, &mut errors);
                }
                let dir = package_dir(&d.package);
                let dest = join_dir(dir, d.into.as_deref());
                if dest.is_empty() {
                    errors.push(format!(
                        "{label}: import targets the monorepo root; declare it in a sub-package or set `into`"
                    ));
                }
                let patches = d
                    .patches
                    .iter()
                    .map(|p| {
                        validate::check_rel_path(&label, "patch", p, &mut errors);
                        join_dir(dir, Some(p))
                    })
                    .collect();
                imports.push(Import {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    repo: d.repo.clone(),
                    git_ref: d.git_ref.clone(),
                    dest,
                    patches,
                });
            }
            Decl::Export(d) => {
                let label = format!("{}:{}", d.package, d.name);
                validate::check_slug(&label, &d.repo, &mut errors);
                if d.branch.is_empty() {
                    errors.push(format!("{label}: branch is empty"));
                }
                if let Some(from) = &d.from_path {
                    validate::check_rel_path(&label, "from_path", from, &mut errors);
                }
                let dir = package_dir(&d.package);
                let from = join_dir(dir, d.from_path.as_deref());
                if from.is_empty() {
                    errors.push(format!(
                        "{label}: export sources the monorepo root; declare it in a sub-package or set `from_path`"
                    ));
                }
                exports.push(Export {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    repo: d.repo.clone(),
                    branch: d.branch.clone(),
                    from,
                });
            }
        }
    }

    // --- cross-rule checks ---
    // Names are unique per package across all rule kinds; destinations (imports
    // and vendors both write into the tree) must not overlap.
    let names: Vec<(&str, &str, &str)> = imports
        .iter()
        .map(|i| (i.package.as_str(), i.name.as_str(), i.label.as_str()))
        .chain(
            vendors
                .iter()
                .map(|v| (v.package.as_str(), v.name.as_str(), v.label.as_str())),
        )
        .chain(
            exports
                .iter()
                .map(|e| (e.package.as_str(), e.name.as_str(), e.label.as_str())),
        )
        .collect();
    check_unique_names(&names, &mut errors);

    let dests: Vec<(&str, &str)> = imports
        .iter()
        .map(|i| (i.label.as_str(), i.dest.as_str()))
        .chain(vendors.iter().map(|v| (v.label.as_str(), v.dest.as_str())))
        .collect();
    validate::check_destination_overlap(&dests, &mut errors);

    if !errors.is_empty() || monorepo.is_none() {
        errors.sort();
        errors.dedup();
        return Err(errors);
    }

    Ok(Ir {
        monorepo: monorepo.expect("checked above"),
        imports,
        vendors,
        exports,
    })
}

/// Names must be unique per package across all rule kinds. `entries` are
/// `(package, name, label)`.
fn check_unique_names(entries: &[(&str, &str, &str)], errors: &mut Vec<String>) {
    let mut seen: Vec<(String, String)> = Vec::new();
    for &(package, name, label) in entries {
        if seen
            .iter()
            .any(|(p, n)| p.as_str() == package && n.as_str() == name)
        {
            errors.push(format!("duplicate rule name in package {package}: {label}"));
        } else {
            seen.push((package.to_string(), name.to_string()));
        }
    }
}

#[cfg(test)]
mod tests;
