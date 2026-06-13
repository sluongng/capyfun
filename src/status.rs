//! Dry-run reconcile: compute each target's *desired vs actual* sync state
//! without mutating anything.
//!
//! This is the read-only sibling of `reconcile`. For every `github_import`,
//! `git_repository`, and `github_export` in the IR it answers "up to date / N
//! behind / pin changed?" by comparing the *desired* upstream state against the
//! *actual* state recorded in the commit map (the CapyFun trailers). The
//! comparison is exactly the level-triggered diff the reconciler acts on (see
//! `docs/design/automation.md`): desired := `ls-remote` the tracked ref (import)
//! / the declared pin (vendor) / the monorepo tip (export); actual := the last
//! synced commit from the trailer scan.
//!
//! Git/network I/O is delegated to [`crate::engine`] and [`crate::vendorgen`];
//! this module only orchestrates and never moves a ref.

use git2::{Oid, Repository};

use crate::engine;
use crate::ir::{Export, Import, Ir, Vendor};
use crate::vendorgen;

/// Which kind of edge a [`TargetStatus`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Import,
    Vendor,
    Export,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Import => "import",
            TargetKind::Vendor => "vendor",
            TargetKind::Export => "export",
        }
    }
}

/// The reconcile state of one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Actual matches desired: a reconcile would be a no-op.
    UpToDate,
    /// Upstream/monorepo has commits the target has not synced yet. `count` is
    /// the number of new first-parent commits (import) or changesets to ship
    /// (export), or `None` if it could not be counted (e.g. history was
    /// rewritten upstream). `new` marks a target that has never been synced.
    Behind { count: Option<usize>, new: bool },
    /// A pinned vendor whose declared `commit` differs from what is vendored
    /// (including never-vendored). `new` marks a never-vendored target.
    PinChanged { new: bool },
    /// Resolving the desired or actual state failed (network, malformed ref, …).
    Error(String),
}

impl State {
    /// Whether a reconcile would actually change the monorepo.
    pub fn is_actionable(&self) -> bool {
        matches!(self, State::Behind { .. } | State::PinChanged { .. })
    }
}

/// The computed status of one import/vendor/export target.
#[derive(Debug, Clone)]
pub struct TargetStatus {
    pub label: String,
    pub kind: TargetKind,
    /// The upstream/destination `owner/name` slug.
    pub repo: String,
    /// Desired state, for display: upstream tip SHA / declared pin / monorepo tip.
    pub desired: Option<String>,
    /// Actual synced state, for display: last imported/vendored/exported commit.
    pub actual: Option<String>,
    pub state: State,
}

impl TargetStatus {
    /// A one-line human summary, e.g. `3 commit(s) behind` or `up to date`.
    pub fn summary(&self) -> String {
        match &self.state {
            State::UpToDate => "up to date".to_owned(),
            State::Behind { count, new } => {
                let what = match self.kind {
                    TargetKind::Export => "change(s) to ship",
                    _ => "commit(s) behind",
                };
                let prefix = if *new { "uninitialized; " } else { "" };
                match count {
                    Some(n) => format!("{prefix}{n} {what}"),
                    None => format!("{prefix}behind ({what}, count unknown)"),
                }
            }
            State::PinChanged { new } => {
                if *new {
                    "uninitialized; pin to vendor".to_owned()
                } else {
                    "pin changed; re-vendor".to_owned()
                }
            }
            State::Error(e) => format!("error: {e}"),
        }
    }
}

/// Compute the status of every target in `ir`, optionally restricted to a single
/// `label`. Targets whose desired/actual state cannot be resolved are reported as
/// [`State::Error`] rather than aborting the whole sweep.
pub fn status_all(repo: &Repository, ir: &Ir, label: Option<&str>) -> Vec<TargetStatus> {
    let mut out = Vec::new();
    for i in &ir.imports {
        if label.is_none_or(|l| l == i.label) {
            out.push(import_status(repo, ir, i));
        }
    }
    for v in &ir.vendors {
        if label.is_none_or(|l| l == v.label) {
            out.push(vendor_status(repo, ir, v));
        }
    }
    for e in &ir.exports {
        if label.is_none_or(|l| l == e.label) {
            out.push(export_status(repo, ir, e));
        }
    }
    out
}

/// The current tip of the monorepo's default branch, or `None` if unborn.
fn branch_tip(repo: &Repository, default_branch: &str) -> Option<Oid> {
    repo.find_reference(&format!("refs/heads/{default_branch}"))
        .ok()
        .and_then(|r| r.target())
}

/// Resolve a remote ref name (e.g. `refs/heads/main`) to its tip SHA via an
/// anonymous `ls-remote`, without fetching objects. `None` if the ref is absent.
fn resolve_remote_ref(slug: &str, git_ref: &str) -> anyhow::Result<Option<String>> {
    let refs = vendorgen::ls_remote(slug)?;
    Ok(refs
        .into_iter()
        .find(|(n, _)| n == git_ref)
        .map(|(_, sha)| sha))
}

fn import_status(repo: &Repository, ir: &Ir, import: &Import) -> TargetStatus {
    let mk = |desired, actual, state| TargetStatus {
        label: import.label.clone(),
        kind: TargetKind::Import,
        repo: import.repo.clone(),
        desired,
        actual,
        state,
    };

    let base = branch_tip(repo, &ir.monorepo.default_branch);
    let actual = match engine::last_imported_origin(repo, base, &import.dest) {
        Ok(a) => a,
        Err(e) => return mk(None, None, State::Error(format!("{e:#}"))),
    };
    let actual_hex = actual.map(|o| o.to_string());

    // Desired: ls-remote the tracked ref (cheap; no object fetch).
    let desired = match resolve_remote_ref(&import.repo, &import.git_ref) {
        Ok(Some(sha)) => sha,
        Ok(None) => {
            return mk(
                None,
                actual_hex,
                State::Error(format!("ref `{}` not found on {}", import.git_ref, import.repo)),
            )
        }
        Err(e) => return mk(None, actual_hex, State::Error(format!("{e:#}"))),
    };

    if actual_hex.as_deref() == Some(desired.as_str()) {
        return mk(Some(desired), actual_hex, State::UpToDate);
    }

    // Behind: fetch the origin tip and count the first-parent delta past `actual`.
    let count = count_import_delta(repo, &import.repo, &import.git_ref, actual);
    mk(
        Some(desired),
        actual_hex,
        State::Behind {
            count,
            new: actual.is_none(),
        },
    )
}

/// Number of new first-parent origin commits past `last` (fetching the tracked
/// ref into the object store first). `None` if the fetch or walk fails (e.g. the
/// upstream history was rewritten so `last` is no longer on the chain).
fn count_import_delta(
    repo: &Repository,
    slug: &str,
    git_ref: &str,
    last: Option<Oid>,
) -> Option<usize> {
    let url = vendorgen::github_url(slug);
    let tip = engine::fetch_commit(repo, &url, git_ref).ok()?;
    engine::first_parent_delta(repo, tip, last)
        .ok()
        .map(|v| v.len())
}

fn vendor_status(repo: &Repository, ir: &Ir, vendor: &Vendor) -> TargetStatus {
    let base = branch_tip(repo, &ir.monorepo.default_branch);
    let actual = match engine::last_vendored(repo, base, &vendor.dest) {
        Ok(a) => a,
        Err(e) => {
            return TargetStatus {
                label: vendor.label.clone(),
                kind: TargetKind::Vendor,
                repo: vendor.repo.clone(),
                desired: Some(vendor.commit.clone()),
                actual: None,
                state: State::Error(format!("{e:#}")),
            }
        }
    };
    let actual_hex = actual.map(|o| o.to_string());

    // Desired is the declared pin — no network needed for a pinned snapshot.
    let state = if actual_hex.as_deref() == Some(vendor.commit.as_str()) {
        State::UpToDate
    } else {
        State::PinChanged {
            new: actual.is_none(),
        }
    };
    TargetStatus {
        label: vendor.label.clone(),
        kind: TargetKind::Vendor,
        repo: vendor.repo.clone(),
        desired: Some(vendor.commit.clone()),
        actual: actual_hex,
        state,
    }
}

fn export_status(repo: &Repository, ir: &Ir, export: &Export) -> TargetStatus {
    let mk = |desired, actual, state| TargetStatus {
        label: export.label.clone(),
        kind: TargetKind::Export,
        repo: export.repo.clone(),
        desired,
        actual,
        state,
    };

    let Some(mono_tip) = branch_tip(repo, &ir.monorepo.default_branch) else {
        return mk(
            None,
            None,
            State::Error("monorepo branch has no commits to export".to_owned()),
        );
    };
    let mono_hex = Some(mono_tip.to_string());

    // Actual: the destination branch is the export commit map. Fetch it (absent
    // branch ⇒ nothing has shipped yet).
    let url = vendorgen::github_url(&export.repo);
    let dest_tip =
        engine::fetch_commit(repo, &url, &format!("refs/heads/{}", export.branch)).ok();
    let last = match engine::last_exported(repo, dest_tip) {
        Ok(l) => l,
        Err(e) => return mk(mono_hex, None, State::Error(format!("{e:#}"))),
    };
    let last_hex = last.map(|o| o.to_string());

    if last == Some(mono_tip) {
        return mk(mono_hex, last_hex, State::UpToDate);
    }

    // Behind: project the export (writes objects, moves no ref) to count the
    // changesets that would ship.
    let count = engine::export(repo, &export.from, mono_tip, dest_tip, &export.transforms)
        .ok()
        .map(|o| o.exported);
    mk(
        mono_hex,
        last_hex,
        State::Behind {
            count,
            new: last.is_none(),
        },
    )
}

#[cfg(test)]
mod tests;
