//! Git rewrite/projection engine.
//!
//! Houses the JOSH-shaped tree-prefix primitive and (in later milestones) the
//! commit replay logic for import. This is the only module that performs Git
//! I/O.

use std::path::Path;

use anyhow::{Context, Result};
use git2::{Commit, Diff, DiffFormat, Oid, Repository, Signature, Time, Tree};

use crate::transform::Transform;

pub mod agent_exec;

pub use agent_exec::{AgentInvocation, AgentRunner, LiveRunner, PromptContext};

/// Git filemode for a tree (subdirectory) entry.
const FILEMODE_TREE: i32 = 0o040000;
/// Git filemode for a regular (non-executable) file blob.
const FILEMODE_BLOB: i32 = 0o100644;
/// Git filemode for an executable file blob.
const FILEMODE_BLOB_EXECUTABLE: i32 = 0o100755;

/// Commit-message trailer recording the origin commit a mirror commit reflects.
/// This trailer is the durable commit map: greppable, clone-surviving, and the
/// basis for incremental import.
pub const ORIGIN_TRAILER: &str = "CapyFun-Origin";

/// Commit-message trailer scoping a CapyFun commit to its import destination, so
/// several imports can share one branch without conflating their commit maps.
pub const IMPORT_TRAILER: &str = "CapyFun-Import";

/// Commit-message trailer marking a CapyFun-authored patch-layer commit.
pub const PATCH_TRAILER: &str = "CapyFun-Patch";

/// Commit-message trailer marking a CapyFun-authored `agent_transform` commit.
/// Its value is the agent's label (e.g. `//tools/agent:porter`).
pub const AGENT_TRAILER: &str = "CapyFun-Agent";

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
    transforms: &[Transform],
) -> Result<Oid> {
    let origin_commit = repo
        .find_commit(origin)
        .with_context(|| format!("origin commit {origin} not found"))?;
    // The imported subtree, upstream-shaped: rename `SRC` markers, then apply
    // structural transforms (move/copy/replace) before it is spliced under
    // `dest`, so all transform paths stay subtree-relative.
    let origin_tree = rename_src_markers(repo, origin_commit.tree()?.id())?;
    let origin_tree = apply_structural_tree_transforms(repo, origin_tree, transforms)
        .with_context(|| format!("applying transforms to origin commit {origin}"))?;

    let base_tree = match parent {
        Some(p) => repo.find_commit(p)?.tree()?.id(),
        None => empty_tree(repo)?,
    };
    let dest_subtree = dest_subtree_preserving_metadata(repo, base_tree, dest, origin_tree)?;
    let new_tree_oid = splice_tree(repo, base_tree, dest, dest_subtree)?;
    let new_tree = repo.find_tree(new_tree_oid)?;

    // Rewrite the message (strip/add trailers, optional body sub) before the
    // engine appends its own CapyFun-Origin / CapyFun-Import trailers.
    let rewritten = apply_message_transforms(origin_commit.message().unwrap_or(""), transforms)
        .with_context(|| format!("rewriting message of origin commit {origin}"))?;
    let message = with_mirror_trailers(&rewritten, origin, dest);

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
    transforms: &[Transform],
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
        head = Some(replay_commit(repo, dest, *origin, head, transforms)?);
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

// --- M8: export (publish a monorepo path out as a GitHub PR) ---

/// Commit-message trailer recording the monorepo commit a destination commit
/// reflects. This is the export side of the commit map — the inverse of
/// [`ORIGIN_TRAILER`]: it lets a re-export know what has already shipped, so
/// export is incremental and idempotent the same way import is.
pub const EXPORT_TRAILER: &str = "CapyFun-Export";

/// Restore `ORIG_SRC` files (at any depth) back to `SRC`, the inverse of
/// [`rename_src_markers`]. Used on the export path so a subtree that once carried
/// an upstream `SRC` (renamed to `ORIG_SRC` on import) ships back out with its
/// own `SRC` intact. Deterministic: a tree with no `ORIG_SRC` rewrites to an
/// identical OID.
fn restore_src_markers(repo: &Repository, tree_oid: Oid) -> Result<Oid> {
    let tree = repo.find_tree(tree_oid)?;
    let mut builder = repo.treebuilder(None)?;
    for entry in tree.iter() {
        let name = entry.name().context("non-UTF-8 tree entry name")?;
        let mode = entry.filemode();
        if entry.kind() == Some(git2::ObjectType::Tree) {
            let rewritten = restore_src_markers(repo, entry.id())?;
            builder.insert(name, rewritten, mode)?;
        } else if name == RENAMED_SRC_FILE {
            builder.insert(SRC_FILE, entry.id(), mode)?;
        } else {
            builder.insert(name, entry.id(), mode)?;
        }
    }
    Ok(builder.write()?)
}

/// Strip CapyFun metadata from an exported subtree so the destination sees only
/// shippable content — the inverse of import's metadata handling. CapyFun's own
/// package marker (`SRC`) and `patches/` directory at the export root are dropped
/// (they are config, not destination source), then any `ORIG_SRC` is restored to
/// `SRC` (see [`restore_src_markers`]). Order matters: the CapyFun `SRC` is
/// removed first, then a buried upstream `SRC` (held as `ORIG_SRC`) is restored
/// into its place.
fn strip_capyfun_metadata_for_export(repo: &Repository, subtree: Oid) -> Result<Oid> {
    let tree = repo.find_tree(subtree)?;
    let mut builder = repo.treebuilder(Some(&tree))?;
    for name in RESERVED_DEST_ENTRIES {
        if builder.get(name)?.is_some() {
            builder.remove(name)?;
        }
    }
    let stripped = builder.write()?;
    restore_src_markers(repo, stripped)
}

/// Append the [`EXPORT_TRAILER`] (recording the monorepo commit) to a message.
fn with_export_trailer(message: &str, mono: Oid) -> String {
    let body = message.trim_end();
    let trailer = format!("{EXPORT_TRAILER}: {mono}\n");
    if body.is_empty() {
        trailer
    } else {
        format!("{body}\n\n{trailer}")
    }
}

/// The last monorepo commit reflected on the destination, from the most recent
/// [`EXPORT_TRAILER`] along `dest_tip`'s first-parent chain. Each destination
/// repository is dedicated to one export (the prefix is stripped), so no further
/// scoping is needed. Returns `None` when nothing has shipped yet.
fn last_exported(repo: &Repository, dest_tip: Option<Oid>) -> Result<Option<Oid>> {
    let mut cur = dest_tip;
    while let Some(c) = cur {
        let commit = repo.find_commit(c)?;
        if let Some(sha) = trailer_value(commit.message().unwrap_or(""), EXPORT_TRAILER) {
            let oid = Oid::from_str(&sha)
                .with_context(|| format!("commit {c} has malformed {EXPORT_TRAILER}: {sha}"))?;
            return Ok(Some(oid));
        }
        cur = commit.parent_ids().next();
    }
    Ok(None)
}

/// Outcome of an export run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    /// Destination commits created this run (commits whose `from` content was
    /// unchanged are skipped, so this counts only real changes to the path).
    pub exported: usize,
    /// The resulting export-branch tip (unchanged from `dest_tip` when nothing
    /// new shipped). The caller pushes this to the destination and opens a PR.
    pub head: Option<Oid>,
}

/// Project the monorepo's first-parent history for `from` onto the destination —
/// the inverse of [`import_mirror`].
///
/// Every monorepo commit newer than the last-exported one (read from `dest_tip`'s
/// trailers) is replayed onto `dest_tip` with the `from` prefix stripped and
/// CapyFun metadata removed ([`strip_capyfun_metadata_for_export`]),
/// author/committer/message preserved, and an [`EXPORT_TRAILER`] appended.
/// Commits that do not change the exported content are skipped, so the result is
/// a clean changeset for one PR. Re-running with nothing new is a no-op. Returns
/// the new branch tip; the caller pushes it and opens the PR.
pub fn export(
    repo: &Repository,
    from: &str,
    mono_tip: Oid,
    dest_tip: Option<Oid>,
) -> Result<ExportOutcome> {
    let last = last_exported(repo, dest_tip)?;
    if Some(mono_tip) == last {
        return Ok(ExportOutcome {
            exported: 0,
            head: dest_tip,
        });
    }
    let to_export = first_parent_delta(repo, mono_tip, last)?;

    let mut head = dest_tip;
    let mut prev_tree = match dest_tip {
        Some(t) => Some(repo.find_commit(t)?.tree()?.id()),
        None => None,
    };
    let mut exported = 0;
    for mono in &to_export {
        let mono_commit = repo.find_commit(*mono)?;
        let Some(sub) = subtree_oid(repo, mono_commit.tree()?.id(), from)? else {
            // `from` does not exist at this commit (created later): nothing to ship.
            continue;
        };
        let shipped = strip_capyfun_metadata_for_export(repo, sub)?;
        if Some(shipped) == prev_tree {
            // This monorepo commit left the exported content untouched: skip it,
            // so the PR holds only commits that change the destination.
            continue;
        }
        let tree = repo.find_tree(shipped)?;
        let message = with_export_trailer(mono_commit.message().unwrap_or(""), *mono);
        let parent_commit: Option<Commit> = match head {
            Some(h) => Some(repo.find_commit(h)?),
            None => None,
        };
        let parents: Vec<&Commit> = parent_commit.iter().collect();
        head = Some(repo.commit(
            None,
            &mono_commit.author(),
            &mono_commit.committer(),
            &message,
            &tree,
            &parents,
        )?);
        prev_tree = Some(shipped);
        exported += 1;
    }
    Ok(ExportOutcome { exported, head })
}

/// Push `head` to `branch` on the remote at `url`, creating/overwriting that
/// branch. The commit is published through a throwaway local ref (libgit2 push
/// needs a local ref as the refspec source); the ref is removed afterward.
pub fn push_branch(repo: &Repository, url: &str, head: Oid, branch: &str) -> Result<()> {
    let local_ref = "refs/capyfun/export_push_head";
    repo.reference(local_ref, head, true, "capyfun export push")?;
    let mut remote = repo
        .remote_anonymous(url)
        .with_context(|| format!("opening remote {url}"))?;
    let refspec = format!("+{local_ref}:refs/heads/{branch}");
    let result = remote
        .push(&[&refspec], None)
        .with_context(|| format!("pushing {branch} to {url}"));
    if let Ok(mut r) = repo.find_reference(local_ref) {
        r.delete().ok();
    }
    result
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

/// Strip this import's tip-layer commits from the top of `tip`, returning the
/// underlying mirror tip. Both `CapyFun-Patch` (`apply_patch`) and
/// `CapyFun-Agent` (`agent_transform`) commits scoped to this `dest` are removed,
/// so an import whose commits sit at the branch tip can rebase its whole tip
/// layer. (If a *different* import's commits sit on top, this stops at them;
/// re-importing a buried import is out of scope — use per-import refs.)
fn strip_tip_layer(repo: &Repository, tip: Option<Oid>, dest: &str) -> Result<Option<Oid>> {
    let mut cur = tip;
    while let Some(c) = cur {
        let commit = repo.find_commit(c)?;
        let message = commit.message().unwrap_or("");
        let scoped = trailer_value(message, IMPORT_TRAILER).as_deref() == Some(dest);
        let is_our_tip =
            scoped && (has_trailer(message, PATCH_TRAILER) || has_trailer(message, AGENT_TRAILER));
        if is_our_tip {
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

/// A resolved tip-phase transform the engine applies once on top of the mirror.
///
/// This is the engine-facing form of [`Transform::ApplyPatch`] /
/// [`Transform::AgentTransform`]: the caller has read the patch bytes and
/// resolved the agent (harness/model/prompt) so the engine stays decoupled from
/// [`crate::ir`].
#[derive(Clone)]
pub enum TipTransform {
    /// A static unified-diff patch (the `apply_patch` transform / `patches=[]`).
    Patch(PatchFile),
    /// A generative `agent_transform`: run an agent, materialize its edits.
    Agent(AgentInvocation),
}

/// Whether an agent run was served from cache or freshly generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCacheStatus {
    Hit,
    Miss,
}

/// Result of (re)applying the tip layer: the new tip plus a per-kind tally.
#[derive(Debug, Clone, Default)]
pub struct TipOutcome {
    pub head: Option<Oid>,
    /// `apply_patch` commits created.
    pub patch_commits: usize,
    /// `agent_transform` commits created (no-op agent runs are not counted).
    pub agent_commits: usize,
    /// Agent runs served from the content-addressed cache.
    pub agent_cache_hits: usize,
    /// Agent runs that invoked the runner (cache miss / `--refresh`).
    pub agent_cache_misses: usize,
}

/// Apply an ordered tip layer (`apply_patch` + `agent_transform`) on top of
/// `mirror_tip`, returning the new tip and a tally. A patch that fails to apply
/// (or an agent run that errors) aborts with a clear error; since no ref is moved
/// here, nothing is half-written.
///
/// `ctx` supplies the engine-derived prompt context vars; `runner` executes
/// agent transforms; `refresh` bypasses the agent cache.
pub fn apply_tip_layer(
    repo: &Repository,
    dest: &str,
    mirror_tip: Oid,
    tips: &[TipTransform],
    runner: &dyn AgentRunner,
    ctx: &PromptContext,
    refresh: bool,
) -> Result<TipOutcome> {
    let sig = capyfun_signature()?;
    let mut head = mirror_tip;
    let mut outcome = TipOutcome {
        head: Some(mirror_tip),
        ..Default::default()
    };

    for tip in tips {
        let parent = repo.find_commit(head)?;
        match tip {
            TipTransform::Patch(patch) => {
                let new_tree_oid =
                    apply_patch_to_tree(repo, parent.tree()?.id(), dest, &patch.bytes)
                        .with_context(|| format!("applying patch {}", patch.label))?;
                let new_tree = repo.find_tree(new_tree_oid)?;
                let message = format!(
                    "Apply patch {}\n\n{PATCH_TRAILER}: {}\n{IMPORT_TRAILER}: {dest}\n",
                    patch.label, patch.label
                );
                head = repo.commit(None, &sig, &sig, &message, &new_tree, &[&parent])?;
                outcome.patch_commits += 1;
            }
            TipTransform::Agent(inv) => {
                let (patch, status) =
                    materialize_agent_patch(repo, dest, parent.tree()?.id(), inv, runner, ctx, refresh)
                        .with_context(|| format!("running agent_transform {}", inv.agent_id))?;
                match status {
                    AgentCacheStatus::Hit => outcome.agent_cache_hits += 1,
                    AgentCacheStatus::Miss => outcome.agent_cache_misses += 1,
                }
                // An empty patch means the agent made no changes: skip the commit.
                if patch.is_empty() {
                    continue;
                }
                let new_tree_oid = apply_patch_to_tree(repo, parent.tree()?.id(), dest, &patch)
                    .with_context(|| {
                        format!("applying materialized patch for agent {}", inv.agent_id)
                    })?;
                let new_tree = repo.find_tree(new_tree_oid)?;
                let message = format!(
                    "Apply agent_transform {}\n\n{AGENT_TRAILER}: {}\n{IMPORT_TRAILER}: {dest}\n",
                    inv.agent_id, inv.agent_id
                );
                head = repo.commit(None, &sig, &sig, &message, &new_tree, &[&parent])?;
                outcome.agent_commits += 1;
            }
        }
    }
    outcome.head = Some(head);
    Ok(outcome)
}

// --- T5: agent_transform materialization + content-addressed cache ---

/// Directory under the repo's git dir holding materialized agent patches, keyed
/// by content-addressed cache key.
const AGENT_CACHE_DIR: &[&str] = &["capyfun", "agent-cache"];

/// The cache key for an agent run: a blake3 hash of `(parent subtree OID at dest,
/// rendered prompt, agent identity)`. The credential is deliberately excluded
/// (see [`AgentInvocation::identity`]), so rotating a key does not invalidate
/// materialized output. Deterministic in its inputs.
fn agent_cache_key(parent_subtree: Oid, rendered_prompt: &str, inv: &AgentInvocation) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(parent_subtree.as_bytes());
    hasher.update(b"\0");
    hasher.update(rendered_prompt.as_bytes());
    hasher.update(b"\0");
    hasher.update(inv.identity().as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// The on-disk path of a cache entry under `<repo>/.git/capyfun/agent-cache/`.
fn agent_cache_path(repo: &Repository, key: &str) -> std::path::PathBuf {
    let mut p = repo.path().to_path_buf();
    for seg in AGENT_CACHE_DIR {
        p.push(seg);
    }
    p.push(format!("{key}.patch"));
    p
}

/// The destination subtree OID within a tree, or `None` if `dest` is absent.
fn subtree_oid(repo: &Repository, tree: Oid, dest: &str) -> Result<Option<Oid>> {
    let tree = repo.find_tree(tree)?;
    match tree.get_path(Path::new(dest)) {
        Ok(e) if e.kind() == Some(git2::ObjectType::Tree) => Ok(Some(e.id())),
        _ => Ok(None),
    }
}

/// Produce the materialized patch for an `agent_transform`, using the
/// content-addressed cache. On a cache hit (and not `refresh`) the recorded
/// patch is loaded and replayed deterministically; on a miss (or `refresh`) the
/// agent runs in a temp checkout, its edits are captured as a unified diff in
/// pure libgit2, the patch is stored under the cache key, and returned.
///
/// Returns the patch bytes (empty if the agent made no changes) and whether it
/// was a cache hit or miss.
fn materialize_agent_patch(
    repo: &Repository,
    dest: &str,
    parent_tree: Oid,
    inv: &AgentInvocation,
    runner: &dyn AgentRunner,
    ctx: &PromptContext,
    refresh: bool,
) -> Result<(Vec<u8>, AgentCacheStatus)> {
    let before = subtree_oid(repo, parent_tree, dest)?
        .with_context(|| format!("destination `{dest}` not found for agent {}", inv.agent_id))?;
    let prompt = agent_exec::render_prompt(&inv.prompt, ctx);
    let key = agent_cache_key(before, &prompt, inv);
    let cache_path = agent_cache_path(repo, &key);

    if !refresh && cache_path.exists() {
        let bytes = std::fs::read(&cache_path)
            .with_context(|| format!("reading cached agent patch {}", cache_path.display()))?;
        return Ok((bytes, AgentCacheStatus::Hit));
    }

    // Cache miss (or refresh): check the subtree out, run the agent, diff back.
    let workdir = tempfile::tempdir().context("creating agent workdir")?;
    checkout_tree_to_dir(repo, before, workdir.path())
        .with_context(|| format!("checking out subtree for agent {}", inv.agent_id))?;
    runner
        .run(inv, &prompt, workdir.path())
        .with_context(|| format!("agent {} run", inv.agent_id))?;
    let after = read_dir_to_tree(repo, workdir.path())
        .with_context(|| format!("reading edited subtree for agent {}", inv.agent_id))?;

    let patch = diff_trees_to_patch(repo, before, after)?;

    // Store the materialized patch (the durable artifact) under the cache key.
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating agent cache dir {}", parent.display()))?;
    }
    std::fs::write(&cache_path, &patch)
        .with_context(|| format!("writing agent patch {}", cache_path.display()))?;

    Ok((patch, AgentCacheStatus::Miss))
}

/// Materialize the blobs of `tree` into `dir` on disk (recursively), so a harness
/// can edit them in place. Only regular/executable file blobs are written;
/// symlinks/submodules are skipped (out of scope for agent edits).
fn checkout_tree_to_dir(repo: &Repository, tree: Oid, dir: &Path) -> Result<()> {
    let tree = repo.find_tree(tree)?;
    for entry in tree.iter() {
        let name = entry.name().context("non-UTF-8 tree entry name")?;
        let path = dir.join(name);
        match entry.kind() {
            Some(git2::ObjectType::Tree) => {
                std::fs::create_dir_all(&path)
                    .with_context(|| format!("creating {}", path.display()))?;
                checkout_tree_to_dir(repo, entry.id(), &path)?;
            }
            Some(git2::ObjectType::Blob) => {
                let blob = repo.find_blob(entry.id())?;
                std::fs::write(&path, blob.content())
                    .with_context(|| format!("writing {}", path.display()))?;
                set_executable(&path, entry.filemode() == FILEMODE_BLOB_EXECUTABLE)?;
            }
            // Symlinks, submodules: not editable subtree content; skip.
            _ => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path, exec: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if exec {
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _exec: bool) -> Result<()> {
    Ok(())
}

/// Read a directory tree from disk back into a Git tree object, returning its
/// OID. The inverse of [`checkout_tree_to_dir`]; this is the "after" tree used to
/// diff the agent's edits. Files keep their executable bit; directories recurse.
fn read_dir_to_tree(repo: &Repository, dir: &Path) -> Result<Oid> {
    let mut builder = repo.treebuilder(None)?;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading dir {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    // Sort for determinism (treebuilder order does not matter for the OID, but a
    // stable walk keeps behavior predictable).
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF-8 filename in agent workdir"))?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let child = read_dir_to_tree(repo, &path)?;
            // Skip empty directories (Git does not track them).
            if !repo.find_tree(child)?.is_empty() {
                builder.insert(&name, child, FILEMODE_TREE)?;
            }
        } else if file_type.is_file() {
            let content = std::fs::read(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let oid = repo.blob(&content)?;
            let mode = if is_executable(&path)? {
                FILEMODE_BLOB_EXECUTABLE
            } else {
                FILEMODE_BLOB
            };
            builder.insert(&name, oid, mode)?;
        }
        // Symlinks etc.: skip (not editable subtree content).
    }
    Ok(builder.write()?)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    Ok(mode & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> Result<bool> {
    Ok(false)
}

/// Diff two subtrees into a unified-diff patch buffer compatible with
/// [`apply_patch_to_tree`] / [`strip_patch_preamble`], in pure libgit2. The
/// paths in the patch are subtree-relative (no `dest` prefix), so the result
/// applies within `dest` exactly like a static `apply_patch` patch. An empty
/// patch (`before == after`) yields empty bytes.
fn diff_trees_to_patch(repo: &Repository, before: Oid, after: Oid) -> Result<Vec<u8>> {
    if before == after {
        return Ok(Vec::new());
    }
    let before_tree = repo.find_tree(before)?;
    let after_tree = repo.find_tree(after)?;
    let diff = repo.diff_tree_to_tree(Some(&before_tree), Some(&after_tree), None)?;
    let mut buf = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            buf.push(line.origin() as u8);
        }
        buf.extend_from_slice(line.content());
        true
    })?;
    Ok(buf)
}

/// Outcome of a full import, including the tip-layer tally.
#[derive(Debug, Clone)]
pub struct FullImportOutcome {
    /// Mirror commits imported this run.
    pub imported: usize,
    /// The resulting tip after the tip layer is (re)applied.
    pub head: Option<Oid>,
    /// Per-kind tip-layer tally (patch/agent commits, cache hits/misses).
    pub tip: TipOutcome,
}

/// The tip-layer inputs and execution policy for an [`import`]: the static
/// `patches=[]` series (applied first), the declared tip transforms (applied in
/// order after), the [`AgentRunner`] that executes `agent_transform`s, and
/// whether to bypass the agent cache (`refresh`). Bundled into one struct so the
/// [`import`] signature stays small.
pub struct TipLayer<'a> {
    /// The `patches = [...]` series, applied first as `CapyFun-Patch` commits.
    pub patches: &'a [PatchFile],
    /// Declared tip transforms (`apply_patch` + `agent_transform`), in order.
    pub tips: &'a [TipTransform],
    /// Executes `agent_transform`s (the live harness, or a fake in tests).
    pub runner: &'a dyn AgentRunner,
    /// Bypass the content-addressed agent cache and re-run every agent.
    pub refresh: bool,
}

/// Full import: mirror the origin's first-parent history, then (re)apply the tip
/// layer (`patches=[]` first, then the ordered tip transforms) on top.
///
/// Idempotent — re-running with no new upstream commits and the same tip
/// transforms reproduces the same tip OID. The tip layer is dropped (both
/// `CapyFun-Patch` and `CapyFun-Agent` commits) and deterministically
/// re-applied; agent output is replayed from the content-addressed cache, so a
/// no-op re-import yields identical OIDs.
pub fn import(
    repo: &Repository,
    dest: &str,
    origin_tip: Oid,
    branch_tip: Option<Oid>,
    transforms: &[Transform],
    tip_layer: &TipLayer,
) -> Result<FullImportOutcome> {
    let mirror_base = strip_tip_layer(repo, branch_tip, dest)?;
    let mirror = import_mirror(repo, dest, origin_tip, mirror_base, transforms)?;

    // Build the prompt context from the newest mirror commit + the dest subtree.
    let ctx = match mirror.head {
        Some(tip) => prompt_context(repo, dest, tip)?,
        None => PromptContext::default(),
    };

    // The tip layer is the `patches=[]` series first, then the ordered tip
    // transforms — a single combined list applied in one pass so ordering and
    // the running parent stay correct.
    let combined: Vec<TipTransform> = tip_layer
        .patches
        .iter()
        .cloned()
        .map(TipTransform::Patch)
        .chain(tip_layer.tips.iter().cloned())
        .collect();

    let (head, tip_outcome) = if combined.is_empty() {
        (
            mirror.head,
            TipOutcome {
                head: mirror.head,
                ..Default::default()
            },
        )
    } else {
        let mirror_tip = mirror
            .head
            .context("cannot apply tip layer: the import produced no commits")?;
        let outcome = apply_tip_layer(
            repo,
            dest,
            mirror_tip,
            &combined,
            tip_layer.runner,
            &ctx,
            tip_layer.refresh,
        )?;
        (outcome.head, outcome)
    };

    Ok(FullImportOutcome {
        imported: mirror.imported,
        head,
        tip: tip_outcome,
    })
}

/// Build the typed prompt context for an `agent_transform` from the newest mirror
/// commit (`mirror_tip`) and the destination subtree. `incoming_diff` is the
/// newest mirror commit's diff against its first parent (best-effort: empty for a
/// root commit); `repo_context`/`changed_files` are the subtree's file list
/// (best-effort — a full include-set selection is future work, see the open
/// questions in `docs/design/transformations.md`).
fn prompt_context(repo: &Repository, dest: &str, mirror_tip: Oid) -> Result<PromptContext> {
    let tip = repo.find_commit(mirror_tip)?;
    let origin_commit = parse_origin_trailer(tip.message().unwrap_or("")).unwrap_or_default();
    let origin_message = tip
        .message()
        .unwrap_or("")
        .lines()
        .take_while(|l| !l.trim_start().starts_with(ORIGIN_TRAILER))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();

    // The dest subtree file list (best-effort `repo_context`/`changed_files`).
    let files = match subtree_oid(repo, tip.tree()?.id(), dest)? {
        Some(sub) => subtree_file_list(repo, sub)?,
        None => Vec::new(),
    };
    let file_list = files.join("\n");

    // `incoming_diff`: the newest mirror commit's diff vs its first parent.
    let incoming_diff = match tip.parent(0).ok() {
        Some(parent) => {
            let diff =
                repo.diff_tree_to_tree(Some(&parent.tree()?), Some(&tip.tree()?), None)?;
            let mut buf = Vec::new();
            diff.print(DiffFormat::Patch, |_d, _h, line| {
                if matches!(line.origin(), '+' | '-' | ' ') {
                    buf.push(line.origin() as u8);
                }
                buf.extend_from_slice(line.content());
                true
            })?;
            String::from_utf8_lossy(&buf).into_owned()
        }
        None => String::new(),
    };

    Ok(PromptContext {
        origin_commit,
        origin_message,
        changed_files: file_list.clone(),
        incoming_diff,
        repo_context: file_list,
    })
}

/// The sorted subtree-relative blob paths of `subtree`.
fn subtree_file_list(repo: &Repository, subtree: Oid) -> Result<Vec<String>> {
    let mut out = Vec::new();
    list_blobs(repo, subtree, "", &mut out)?;
    out.sort();
    Ok(out)
}

fn list_blobs(repo: &Repository, tree: Oid, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let tree = repo.find_tree(tree)?;
    for entry in tree.iter() {
        let name = entry.name().context("non-UTF-8 tree entry name")?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.kind() {
            Some(git2::ObjectType::Tree) => list_blobs(repo, entry.id(), &path, out)?,
            Some(git2::ObjectType::Blob) => out.push(path),
            _ => {}
        }
    }
    Ok(())
}

// --- T2: structural transforms (mirror-time, applied per replayed commit) ---
//
// Structural transforms (`move`/`copy`/`replace`) rewrite the imported subtree
// (upstream-shaped, all paths subtree-relative) before it is spliced under the
// destination prefix; `rewrite_message` rewrites the commit message. They run on
// every replayed commit, so the layout/content/message is consistent throughout
// the mirrored history (filter-repo style), and the rewrite is deterministic:
// the same input tree and transforms always yield the same output OID.

/// Apply the tree-rewriting transforms (`move`/`copy`/`replace`) of a pipeline
/// to `subtree`, in order, returning the rewritten subtree OID. Message
/// transforms are ignored here (see [`apply_message_transforms`]).
fn apply_structural_tree_transforms(
    repo: &Repository,
    subtree: Oid,
    transforms: &[Transform],
) -> Result<Oid> {
    let mut tree = subtree;
    for t in transforms {
        tree = match t {
            Transform::Move { src, dst } => move_or_copy(repo, tree, src, dst, true)?,
            Transform::Copy { src, dst } => move_or_copy(repo, tree, src, dst, false)?,
            Transform::Replace {
                before,
                after,
                paths,
                regex,
            } => apply_replace(repo, tree, before, after, paths, *regex)?,
            // Message-only transforms do not touch the tree.
            Transform::RewriteMessage { .. } => tree,
            // Tip-phase transforms are applied in the tip layer, not here.
            Transform::ApplyPatch { .. } | Transform::AgentTransform { .. } => tree,
        };
    }
    Ok(tree)
}

/// Read the tree entry (object id + filemode) at the slash-separated subtree path
/// `rel`, or `None` if no entry exists there.
fn entry_at(repo: &Repository, tree: Oid, rel: &str) -> Result<Option<(Oid, i32)>> {
    let tree = repo.find_tree(tree)?;
    match tree.get_path(Path::new(rel)) {
        Ok(e) => Ok(Some((e.id(), e.filemode()))),
        Err(_) => Ok(None),
    }
}

/// Rebuild `tree` with `rel` set to `entry` (insert/replace) or removed
/// (`entry == None`), creating or pruning intermediate directories as needed.
/// Returns the new tree OID. Determinism: identical inputs yield identical OIDs.
fn set_path(repo: &Repository, tree: Oid, rel: &str, entry: Option<(Oid, i32)>) -> Result<Oid> {
    let comps: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    anyhow::ensure!(!comps.is_empty(), "transform path must not be empty");
    set_path_rec(repo, Some(&repo.find_tree(tree)?), &comps, entry)
}

fn set_path_rec(
    repo: &Repository,
    base: Option<&Tree>,
    comps: &[&str],
    entry: Option<(Oid, i32)>,
) -> Result<Oid> {
    let (head, rest) = comps.split_first().expect("non-empty comps");
    let mut builder = repo.treebuilder(base)?;
    if rest.is_empty() {
        match entry {
            Some((oid, mode)) => builder.insert(head, oid, mode)?,
            None => {
                if builder.get(head)?.is_some() {
                    builder.remove(head)?;
                }
                return Ok(builder.write()?);
            }
        };
    } else {
        let child = base
            .and_then(|t| t.get_name(head))
            .filter(|e| e.kind() == Some(git2::ObjectType::Tree))
            .and_then(|e| repo.find_tree(e.id()).ok());
        let new_child = set_path_rec(repo, child.as_ref(), rest, entry)?;
        // Prune the directory if it became empty (only when removing).
        if entry.is_none() && repo.find_tree(new_child)?.is_empty() {
            if builder.get(head)?.is_some() {
                builder.remove(head)?;
            }
        } else {
            builder.insert(head, new_child, FILEMODE_TREE)?;
        }
    }
    Ok(builder.write()?)
}

/// Move (`remove_src = true`) or copy (`false`) the entry at `src` to `dst`
/// within `subtree`. A `src` that does not exist is an error so a stale
/// `move`/`copy` is loud rather than silently a no-op.
fn move_or_copy(
    repo: &Repository,
    subtree: Oid,
    src: &str,
    dst: &str,
    remove_src: bool,
) -> Result<Oid> {
    let entry = entry_at(repo, subtree, src)?
        .with_context(|| format!("transform source `{src}` does not exist in the subtree"))?;
    let mut tree = set_path(repo, subtree, dst, Some(entry))?;
    if remove_src && src != dst {
        tree = set_path(repo, tree, src, None)?;
    }
    Ok(tree)
}

/// Apply a `replace` to every blob in `subtree` whose subtree-relative path
/// matches one of `paths`. With `regex == false`, `before`/`after` are literal;
/// otherwise `before` is a regex and `after` may use `$N` capture references.
fn apply_replace(
    repo: &Repository,
    subtree: Oid,
    before: &str,
    after: &str,
    paths: &[String],
    regex: bool,
) -> Result<Oid> {
    let re = if regex {
        Some(
            regex::Regex::new(before)
                .with_context(|| format!("invalid replace regex `{before}`"))?,
        )
    } else {
        None
    };
    replace_walk(repo, subtree, "", paths, before, after, re.as_ref())
}

/// Recursively rebuild `tree`, rewriting matching blobs. `prefix` is the
/// subtree-relative directory path accumulated so far (no leading slash).
fn replace_walk(
    repo: &Repository,
    tree: Oid,
    prefix: &str,
    paths: &[String],
    before: &str,
    after: &str,
    re: Option<&regex::Regex>,
) -> Result<Oid> {
    let tree = repo.find_tree(tree)?;
    let mut builder = repo.treebuilder(None)?;
    for entry in tree.iter() {
        let name = entry.name().context("non-UTF-8 tree entry name")?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.kind() == Some(git2::ObjectType::Tree) {
            let rewritten = replace_walk(repo, entry.id(), &path, paths, before, after, re)?;
            builder.insert(name, rewritten, entry.filemode())?;
        } else if entry.kind() == Some(git2::ObjectType::Blob)
            && paths.iter().any(|g| glob_match(g, &path))
        {
            let blob = repo.find_blob(entry.id())?;
            let new_oid = match std::str::from_utf8(blob.content()) {
                Ok(text) => {
                    let replaced = match re {
                        Some(re) => re.replace_all(text, after).into_owned(),
                        None => text.replace(before, after),
                    };
                    if replaced == text {
                        entry.id()
                    } else {
                        repo.blob(replaced.as_bytes())?
                    }
                }
                // Binary file: leave untouched (replace is a text operation).
                Err(_) => entry.id(),
            };
            builder.insert(name, new_oid, entry.filemode())?;
        } else {
            builder.insert(name, entry.id(), entry.filemode())?;
        }
    }
    Ok(builder.write()?)
}

/// Match a slash-separated `path` against a glob `pattern`. Supports `?` (one
/// non-`/` char), `*` (run of non-`/` chars), and `**` (any run including `/`).
/// A leading `**/` also matches zero directories, so `**/*.go` matches `x.go`.
fn glob_match(pattern: &str, path: &str) -> bool {
    // `**/` matching zero directories: also try the pattern with the prefix
    // dropped, so `**/x` matches a top-level `x`.
    if let Some(rest) = pattern.strip_prefix("**/") {
        if glob_match(rest, path) {
            return true;
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = path.chars().collect();
    glob_rec(&p, &t)
}

fn glob_rec(p: &[char], t: &[char]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' if p.get(1) == Some(&'*') => {
            // `**`: match any run, including `/`.
            let rest = &p[2..];
            // Skip an immediately following `/` so `**/x` can match `x` too.
            let rest = rest.strip_prefix(&['/']).unwrap_or(rest);
            (0..=t.len()).any(|i| glob_rec(rest, &t[i..]))
        }
        '*' => {
            // `*`: match a run of non-`/` chars.
            let rest = &p[1..];
            let mut i = 0;
            loop {
                if glob_rec(rest, &t[i..]) {
                    return true;
                }
                if i >= t.len() || t[i] == '/' {
                    return false;
                }
                i += 1;
            }
        }
        '?' => !t.is_empty() && t[0] != '/' && glob_rec(&p[1..], &t[1..]),
        c => !t.is_empty() && t[0] == c && glob_rec(&p[1..], &t[1..]),
    }
}

/// Apply the `rewrite_message` transforms of a pipeline to a commit message, in
/// order. Tree transforms are ignored here. Trailer strips/adds and the optional
/// body substitution are applied; the engine's own trailers are added later.
fn apply_message_transforms(message: &str, transforms: &[Transform]) -> Result<String> {
    let mut msg = message.to_owned();
    for t in transforms {
        if let Transform::RewriteMessage {
            before,
            after,
            regex,
            strip_trailers,
            add_trailers,
        } = t
        {
            msg = rewrite_message(&msg, before, after, *regex, strip_trailers, add_trailers)?;
        }
    }
    Ok(msg)
}

/// Rewrite one message: optional body substitution, then strip the named
/// trailers, then append the given trailer lines.
fn rewrite_message(
    message: &str,
    before: &Option<String>,
    after: &Option<String>,
    regex: bool,
    strip_trailers: &[String],
    add_trailers: &[String],
) -> Result<String> {
    let mut msg = message.to_owned();

    if let (Some(before), Some(after)) = (before, after) {
        msg = if regex {
            let re = regex::Regex::new(before)
                .with_context(|| format!("invalid rewrite_message regex `{before}`"))?;
            re.replace_all(&msg, after.as_str()).into_owned()
        } else {
            msg.replace(before, after)
        };
    }

    if !strip_trailers.is_empty() {
        let kept: Vec<&str> = msg
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !strip_trailers.iter().any(|key| {
                    trimmed
                        .strip_prefix(key)
                        .and_then(|r| r.strip_prefix(':'))
                        .is_some()
                })
            })
            .collect();
        msg = kept.join("\n");
    }

    if !add_trailers.is_empty() {
        let body = msg.trim_end();
        let added = add_trailers.join("\n");
        msg = if body.is_empty() {
            added
        } else {
            format!("{body}\n{added}")
        };
    }

    Ok(msg)
}

#[cfg(test)]
mod tests;
