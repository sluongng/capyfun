//! Git rewrite/projection engine.
//!
//! Houses the JOSH-shaped tree-prefix primitive and (in later milestones) the
//! commit replay logic for import. This is the only module that performs Git
//! I/O.

use std::path::Path;

use anyhow::{Context, Result};
use git2::{Commit, Diff, Oid, Repository, Signature, Time, Tree};

/// Git filemode for a tree (subdirectory) entry.
const FILEMODE_TREE: i32 = 0o040000;

/// Commit-message trailer recording the origin commit a mirror commit reflects.
/// This trailer is the durable commit map: greppable, clone-surviving, and the
/// basis for incremental import.
pub const ORIGIN_TRAILER: &str = "CapyFun-Origin";

/// Commit-message trailer scoping a CapyFun commit to its import destination, so
/// several imports can share one branch without conflating their commit maps.
pub const IMPORT_TRAILER: &str = "CapyFun-Import";

/// Commit-message trailer marking a CapyFun-authored patch-layer commit.
pub const PATCH_TRAILER: &str = "CapyFun-Patch";

/// The value of the last (bottom-most reading, i.e. most recent) `key:` trailer.
fn trailer_value(message: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    message.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(|s| s.trim().to_owned())
    })
}

/// Fixed authorship for CapyFun-authored (patch-layer) commits, so the patch
/// layer is reproducible: re-applying the same patches onto the same mirror tip
/// yields identical commit OIDs.
fn capyfun_signature() -> Result<Signature<'static>> {
    Ok(Signature::new(
        "CapyFun",
        "capyfun@tinytree.dev",
        &Time::new(0, 0),
    )?)
}

/// The empty tree object in `repo` (creating it if absent).
pub fn empty_tree(repo: &Repository) -> Result<Oid> {
    Ok(repo.treebuilder(None)?.write()?)
}

/// Splice `subtree` into `base` at the directory path `dest`, returning the OID
/// of the resulting tree.
///
/// This is the load-bearing rewrite primitive: it takes an origin commit's tree
/// (`subtree`) and places it under the destination prefix (`dest`) inside the
/// monorepo tree (`base`), leaving the rest of `base` untouched. Any existing
/// entry at `dest` is replaced. The operation is content-addressed and
/// deterministic: identical inputs always yield the same OID.
pub fn splice_tree(repo: &Repository, base: Oid, dest: &str, subtree: Oid) -> Result<Oid> {
    let comps: Vec<&str> = dest.split('/').filter(|s| !s.is_empty()).collect();
    anyhow::ensure!(!comps.is_empty(), "splice destination must not be empty");
    let base_tree = repo
        .find_tree(base)
        .with_context(|| format!("base tree {base} not found"))?;
    splice_into(repo, Some(&base_tree), &comps, subtree)
}

/// Recursively rebuild the spine from `dest`'s root down, inserting `subtree` at
/// the final component.
fn splice_into(
    repo: &Repository,
    base: Option<&Tree>,
    comps: &[&str],
    subtree: Oid,
) -> Result<Oid> {
    let (head, rest) = comps.split_first().expect("non-empty comps");

    let new_child = if rest.is_empty() {
        subtree
    } else {
        // Recurse into the existing child tree at `head`, if any.
        let child = base
            .and_then(|t| t.get_name(head))
            .filter(|e| e.kind() == Some(git2::ObjectType::Tree))
            .and_then(|e| repo.find_tree(e.id()).ok());
        splice_into(repo, child.as_ref(), rest, subtree)?
    };

    let mut builder = repo.treebuilder(base)?;
    builder.insert(head, new_child, FILEMODE_TREE)?;
    Ok(builder.write()?)
}

/// Append `CapyFun-Origin` and `CapyFun-Import` trailers to a mirror message.
fn with_mirror_trailers(message: &str, origin: Oid, dest: &str) -> String {
    let body = message.trim_end();
    let trailers = format!("{ORIGIN_TRAILER}: {origin}\n{IMPORT_TRAILER}: {dest}\n");
    if body.is_empty() {
        trailers
    } else {
        format!("{body}\n\n{trailers}")
    }
}

/// Extract the origin SHA from a mirror commit's `CapyFun-Origin` trailer.
pub fn parse_origin_trailer(message: &str) -> Option<String> {
    trailer_value(message, ORIGIN_TRAILER)
}

/// Top-level entries within an import destination that are CapyFun metadata, not
/// imported source: they are preserved across import rather than overwritten by
/// the origin tree. (An upstream repo that itself contains an entry by one of
/// these names within the destination would be shadowed.)
const RESERVED_DEST_ENTRIES: [&str; 2] = ["SRC", "patches"];

/// CapyFun's package-marker filename.
const SRC_FILE: &str = "SRC";
/// Imported `SRC` files are renamed to this so they are not mistaken for CapyFun
/// package markers by discovery.
const RENAMED_SRC_FILE: &str = "ORIG_SRC";

/// Rewrite an imported tree so any file named `SRC` (at any depth) is renamed to
/// `ORIG_SRC`. This keeps an upstream repo's own `SRC` files from being picked up
/// as CapyFun packages once they land in the monorepo. Deterministic: a tree
/// with no `SRC` files rewrites to an identical OID.
fn rename_src_markers(repo: &Repository, tree_oid: Oid) -> Result<Oid> {
    let tree = repo.find_tree(tree_oid)?;
    let mut builder = repo.treebuilder(None)?;
    for entry in tree.iter() {
        let name = entry.name().context("non-UTF-8 tree entry name")?;
        let mode = entry.filemode();
        if entry.kind() == Some(git2::ObjectType::Tree) {
            let rewritten = rename_src_markers(repo, entry.id())?;
            builder.insert(name, rewritten, mode)?;
        } else if name == SRC_FILE {
            builder.insert(RENAMED_SRC_FILE, entry.id(), mode)?;
        } else {
            builder.insert(name, entry.id(), mode)?;
        }
    }
    Ok(builder.write()?)
}

/// Build the destination subtree for a mirror commit: the origin tree, with any
/// reserved CapyFun-metadata entries (see [`RESERVED_DEST_ENTRIES`]) carried over
/// from the existing destination subtree in `base_tree`.
fn dest_subtree_preserving_metadata(
    repo: &Repository,
    base_tree: Oid,
    dest: &str,
    origin_tree: Oid,
) -> Result<Oid> {
    let base = repo.find_tree(base_tree)?;
    let existing = base
        .get_path(Path::new(dest))
        .ok()
        .and_then(|e| e.to_object(repo).ok())
        .and_then(|o| o.into_tree().ok());
    let Some(existing) = existing else {
        return Ok(origin_tree);
    };

    let origin = repo.find_tree(origin_tree)?;
    let mut builder = repo.treebuilder(Some(&origin))?;
    for name in RESERVED_DEST_ENTRIES {
        if let Some(entry) = existing.get_name(name) {
            builder.insert(name, entry.id(), entry.filemode())?;
        }
    }
    Ok(builder.write()?)
}

/// Replay a single origin commit as a mirror commit onto `parent`.
///
/// The origin commit's tree is spliced under `dest` into the parent's tree (or
/// the empty tree when there is no parent), author/committer/message are
/// preserved, and a [`ORIGIN_TRAILER`] is appended. Reserved CapyFun metadata in
/// the destination (`SRC`, `patches`) is carried over. Returns the new commit
/// OID; it does not move any ref.
pub fn replay_commit(
    repo: &Repository,
    dest: &str,
    origin: Oid,
    parent: Option<Oid>,
) -> Result<Oid> {
    let origin_commit = repo
        .find_commit(origin)
        .with_context(|| format!("origin commit {origin} not found"))?;
    let origin_tree = rename_src_markers(repo, origin_commit.tree()?.id())?;

    let base_tree = match parent {
        Some(p) => repo.find_commit(p)?.tree()?.id(),
        None => empty_tree(repo)?,
    };
    let dest_subtree = dest_subtree_preserving_metadata(repo, base_tree, dest, origin_tree)?;
    let new_tree_oid = splice_tree(repo, base_tree, dest, dest_subtree)?;
    let new_tree = repo.find_tree(new_tree_oid)?;

    let message = with_mirror_trailers(origin_commit.message().unwrap_or(""), origin, dest);

    let parent_commit: Option<Commit> = match parent {
        Some(p) => Some(repo.find_commit(p)?),
        None => None,
    };
    let parents: Vec<&Commit> = parent_commit.iter().collect();

    let oid = repo.commit(
        None,
        &origin_commit.author(),
        &origin_commit.committer(),
        &message,
        &new_tree,
        &parents,
    )?;
    Ok(oid)
}

/// Outcome of a mirror import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    /// Number of origin commits mirrored this run.
    pub imported: usize,
    /// The resulting monorepo tip (unchanged from `base` when nothing imported).
    pub head: Option<Oid>,
}

/// The last origin commit reflected for `dest`, found by scanning the
/// first-parent chain of `base` for the most recent mirror commit whose
/// `CapyFun-Import` trailer matches `dest`. Scoping by `dest` lets several
/// imports share one branch without conflating their commit maps. Returns `None`
/// when this import has nothing yet.
fn last_imported_origin(repo: &Repository, base: Option<Oid>, dest: &str) -> Result<Option<Oid>> {
    let mut cur = base;
    while let Some(c) = cur {
        let commit = repo.find_commit(c)?;
        let message = commit.message().unwrap_or("");
        if trailer_value(message, IMPORT_TRAILER).as_deref() == Some(dest) {
            if let Some(sha) = parse_origin_trailer(message) {
                let oid = Oid::from_str(&sha)
                    .with_context(|| format!("commit {c} has malformed {ORIGIN_TRAILER}: {sha}"))?;
                return Ok(Some(oid));
            }
        }
        cur = commit.parent_ids().next();
    }
    Ok(None)
}

/// First-parent commits strictly newer than `stop` on `tip`'s chain, ordered
/// oldest → newest. When `stop` is `None`, returns the whole chain. Errors if
/// `stop` is set but not on the chain (upstream history diverged / was rewritten).
fn first_parent_delta(repo: &Repository, tip: Oid, stop: Option<Oid>) -> Result<Vec<Oid>> {
    let mut chain = Vec::new();
    let mut cur = Some(tip);
    while let Some(c) = cur {
        if Some(c) == stop {
            chain.reverse();
            return Ok(chain);
        }
        chain.push(c);
        cur = repo.find_commit(c)?.parent_ids().next();
    }
    if let Some(stop) = stop {
        anyhow::bail!(
            "origin tip {tip} no longer contains the last imported commit {stop} on its \
             first-parent chain (was history rewritten / force-pushed?)"
        );
    }
    chain.reverse();
    Ok(chain)
}

/// Incrementally mirror the origin's first-parent history into `dest`.
///
/// Replays every first-parent commit newer than the last-imported one (read from
/// `base`'s trailers) as a linear run of mirror commits on top of `base`. Merges
/// are linearized to their first parent. Re-running with no new origin commits is
/// a no-op. Returns the resulting tip; the caller advances any ref.
pub fn import_mirror(
    repo: &Repository,
    dest: &str,
    origin_tip: Oid,
    base: Option<Oid>,
) -> Result<ImportOutcome> {
    let last = last_imported_origin(repo, base, dest)?;
    if Some(origin_tip) == last {
        return Ok(ImportOutcome {
            imported: 0,
            head: base,
        });
    }
    let to_import = first_parent_delta(repo, origin_tip, last)?;
    let mut head = base;
    for origin in &to_import {
        head = Some(replay_commit(repo, dest, *origin, head)?);
    }
    Ok(ImportOutcome {
        imported: to_import.len(),
        head,
    })
}

/// Fetch `want` from `url` into `repo`'s object store and return the commit it
/// resolves to. `want` may be a refname (`refs/heads/main`, `refs/tags/v1`) or a
/// full 40-hex commit SHA (fetched directly, where the server allows it). The
/// objects become available locally so they can be replayed or vendored.
pub fn fetch_commit(repo: &Repository, url: &str, want: &str) -> Result<Oid> {
    let mut remote = repo
        .remote_anonymous(url)
        .with_context(|| format!("opening remote {url}"))?;

    if want.len() == 40 && want.bytes().all(|b| b.is_ascii_hexdigit()) {
        // A pinned commit SHA: fetch the object directly (no ref created).
        remote
            .fetch(&[want], None, None)
            .with_context(|| format!("fetching commit {want} from {url}"))?;
        let oid = Oid::from_str(want)?;
        repo.find_commit(oid)
            .with_context(|| format!("fetched object {want} is not a commit"))?;
        return Ok(oid);
    }

    let tmp = "refs/capyfun/fetch_head";
    remote
        .fetch(&[&format!("+{want}:{tmp}")], None, None)
        .with_context(|| format!("fetching {want} from {url}"))?;
    let oid = repo
        .find_reference(tmp)?
        .target()
        .with_context(|| format!("fetched ref {want} has no target"))?;
    if let Ok(mut r) = repo.find_reference(tmp) {
        r.delete().ok();
    }
    Ok(oid)
}

// --- git_repository: vendor a pinned snapshot (no upstream history) ---

/// Commit-message trailer recording a vendored snapshot's `repo@sha` pin.
pub const VENDOR_TRAILER: &str = "CapyFun-Vendor";

/// The commit a `dest` is currently vendored at, from the most recent
/// `CapyFun-Vendor` trailer scoped to `dest`. `None` if not yet vendored.
fn last_vendored(repo: &Repository, base: Option<Oid>, dest: &str) -> Result<Option<Oid>> {
    let mut cur = base;
    while let Some(c) = cur {
        let commit = repo.find_commit(c)?;
        let message = commit.message().unwrap_or("");
        if trailer_value(message, IMPORT_TRAILER).as_deref() == Some(dest) {
            if let Some(v) = trailer_value(message, VENDOR_TRAILER) {
                if let Some((_, sha)) = v.rsplit_once('@') {
                    return Ok(Some(Oid::from_str(sha)?));
                }
            }
        }
        cur = commit.parent_ids().next();
    }
    Ok(None)
}

/// Vendor a single pinned snapshot of `commit`'s tree into `dest` as one
/// CapyFun-authored commit (no upstream history). Idempotent: re-running with the
/// same commit is a no-op. Reserved metadata (`SRC`, `patches`) in the dest is
/// preserved. `repo_slug` is recorded in the `CapyFun-Vendor` trailer.
pub fn vendor_snapshot(
    repo: &Repository,
    dest: &str,
    repo_slug: &str,
    commit: Oid,
    branch_tip: Option<Oid>,
) -> Result<ImportOutcome> {
    if last_vendored(repo, branch_tip, dest)? == Some(commit) {
        return Ok(ImportOutcome {
            imported: 0,
            head: branch_tip,
        });
    }

    let base_tree = match branch_tip {
        Some(t) => repo.find_commit(t)?.tree()?.id(),
        None => empty_tree(repo)?,
    };
    let snapshot_tree = rename_src_markers(repo, repo.find_commit(commit)?.tree()?.id())?;
    let dest_subtree = dest_subtree_preserving_metadata(repo, base_tree, dest, snapshot_tree)?;
    let new_tree = repo.find_tree(splice_tree(repo, base_tree, dest, dest_subtree)?)?;

    let sig = capyfun_signature()?;
    let message = format!(
        "Vendor {repo_slug}@{commit}\n\n{VENDOR_TRAILER}: {repo_slug}@{commit}\n{IMPORT_TRAILER}: {dest}\n"
    );
    let parent_commit: Option<Commit> = match branch_tip {
        Some(t) => Some(repo.find_commit(t)?),
        None => None,
    };
    let parents: Vec<&Commit> = parent_commit.iter().collect();
    let head = repo.commit(None, &sig, &sig, &message, &new_tree, &parents)?;
    Ok(ImportOutcome {
        imported: 1,
        head: Some(head),
    })
}

// --- M6: patch layer (tip, rebased on the mirror tip) ---

/// A patch to apply in the tip layer: a label (its repo-relative path, recorded
/// in the trailer) and the unified-diff bytes.
#[derive(Debug, Clone)]
pub struct PatchFile {
    pub label: String,
    pub bytes: Vec<u8>,
}

/// Slice a patch buffer to its first `diff --git` (or `--- `) header, dropping
/// any commit-message preamble (Subject/description/`---`) that `git format-patch`
/// emits, which libgit2's diff parser does not expect.
fn strip_patch_preamble(patch: &[u8]) -> &[u8] {
    if let Some(pos) = find_subslice(patch, b"diff --git ") {
        return &patch[pos..];
    }
    if let Some(pos) = find_subslice(patch, b"--- ") {
        return &patch[pos..];
    }
    patch
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .filter(|_| !needle.is_empty())
}

/// Apply one unified-diff patch within `dest` of `base_tree`, returning the new
/// tree OID. The patch's paths are relative to the imported subtree, so it is
/// applied to the subtree at `dest` and the result is spliced back.
pub fn apply_patch_to_tree(
    repo: &Repository,
    base_tree: Oid,
    dest: &str,
    patch: &[u8],
) -> Result<Oid> {
    let base = repo.find_tree(base_tree)?;
    let sub_entry = base
        .get_path(Path::new(dest))
        .with_context(|| format!("destination `{dest}` not found in tree"))?;
    let sub_tree = sub_entry
        .to_object(repo)?
        .into_tree()
        .map_err(|_| anyhow::anyhow!("destination `{dest}` is not a directory"))?;

    let diff = Diff::from_buffer(strip_patch_preamble(patch))
        .context("parsing patch as a unified diff")?;
    let mut index = repo
        .apply_to_tree(&sub_tree, &diff, None)
        .context("patch did not apply cleanly")?;
    let patched_sub = index.write_tree_to(repo)?;
    splice_tree(repo, base_tree, dest, patched_sub)
}

/// Apply an ordered patch series as `CapyFun-Patch` commits on top of
/// `mirror_tip`. Returns the new tip. A patch that fails to apply aborts with a
/// clear error; since no ref is moved here, nothing is half-written.
pub fn apply_patch_layer(
    repo: &Repository,
    dest: &str,
    mirror_tip: Oid,
    patches: &[PatchFile],
) -> Result<Oid> {
    let sig = capyfun_signature()?;
    let mut head = mirror_tip;
    for patch in patches {
        let parent = repo.find_commit(head)?;
        let new_tree_oid = apply_patch_to_tree(repo, parent.tree()?.id(), dest, &patch.bytes)
            .with_context(|| format!("applying patch {}", patch.label))?;
        let new_tree = repo.find_tree(new_tree_oid)?;
        let message = format!(
            "Apply patch {}\n\n{PATCH_TRAILER}: {}\n{IMPORT_TRAILER}: {dest}\n",
            patch.label, patch.label
        );
        head = repo.commit(None, &sig, &sig, &message, &new_tree, &[&parent])?;
    }
    Ok(head)
}

/// Strip this import's patch-layer commits from the top of `tip`, returning the
/// underlying mirror tip. Only commits that are this `dest`'s patch commits are
/// removed, so an import whose commits sit at the branch tip can rebase its own
/// patch layer. (If a *different* import's commits sit on top, this stops at
/// them; re-importing a buried import is out of scope — use per-import refs.)
fn strip_patch_layer(repo: &Repository, tip: Option<Oid>, dest: &str) -> Result<Option<Oid>> {
    let mut cur = tip;
    while let Some(c) = cur {
        let commit = repo.find_commit(c)?;
        let message = commit.message().unwrap_or("");
        let is_our_patch = has_trailer(message, PATCH_TRAILER)
            && trailer_value(message, IMPORT_TRAILER).as_deref() == Some(dest);
        if is_our_patch {
            cur = commit.parent_ids().next();
        } else {
            break;
        }
    }
    Ok(cur)
}

fn has_trailer(message: &str, key: &str) -> bool {
    let prefix = format!("{key}: ");
    message.lines().any(|l| l.trim().starts_with(&prefix))
}

/// Full import: mirror the origin's first-parent history, then (re)apply the
/// patch layer on top. Idempotent — re-running with no new upstream commits and
/// the same patches reproduces the same tip OID (the patch layer is dropped and
/// deterministically re-applied).
pub fn import(
    repo: &Repository,
    dest: &str,
    origin_tip: Oid,
    branch_tip: Option<Oid>,
    patches: &[PatchFile],
) -> Result<ImportOutcome> {
    let mirror_base = strip_patch_layer(repo, branch_tip, dest)?;
    let mirror = import_mirror(repo, dest, origin_tip, mirror_base)?;

    let head = match (mirror.head, patches.is_empty()) {
        (_, true) => mirror.head,
        (Some(mirror_tip), false) => Some(apply_patch_layer(repo, dest, mirror_tip, patches)?),
        (None, false) => {
            anyhow::bail!("cannot apply patches: the import produced no commits")
        }
    };

    Ok(ImportOutcome {
        imported: mirror.imported,
        head,
    })
}

#[cfg(test)]
mod tests;
