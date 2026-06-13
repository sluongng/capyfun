//! Git rewrite/projection engine.
//!
//! Houses the JOSH-shaped tree-prefix primitive and (in later milestones) the
//! commit replay logic for import. This is the only module that performs Git
//! I/O.

use anyhow::{Context, Result};
use git2::{Commit, Oid, Repository, Tree};

/// Git filemode for a tree (subdirectory) entry.
const FILEMODE_TREE: i32 = 0o040000;

/// Commit-message trailer recording the origin commit a mirror commit reflects.
/// This trailer is the durable commit map: greppable, clone-surviving, and the
/// basis for incremental import.
pub const ORIGIN_TRAILER: &str = "CapyFun-Origin";

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

/// Append a `CapyFun-Origin: <sha>` trailer to a commit message.
fn with_origin_trailer(message: &str, origin: Oid) -> String {
    let body = message.trim_end();
    if body.is_empty() {
        format!("{ORIGIN_TRAILER}: {origin}\n")
    } else {
        format!("{body}\n\n{ORIGIN_TRAILER}: {origin}\n")
    }
}

/// Extract the origin SHA from a mirror commit's `CapyFun-Origin` trailer.
pub fn parse_origin_trailer(message: &str) -> Option<String> {
    let prefix = format!("{ORIGIN_TRAILER}: ");
    message.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(|s| s.trim().to_owned())
    })
}

/// Replay a single origin commit as a mirror commit onto `parent`.
///
/// The origin commit's tree is spliced under `dest` into the parent's tree (or
/// the empty tree when there is no parent), author/committer/message are
/// preserved, and a [`ORIGIN_TRAILER`] is appended. Returns the new commit OID;
/// it does not move any ref.
pub fn replay_commit(
    repo: &Repository,
    dest: &str,
    origin: Oid,
    parent: Option<Oid>,
) -> Result<Oid> {
    let origin_commit = repo
        .find_commit(origin)
        .with_context(|| format!("origin commit {origin} not found"))?;
    let origin_tree = origin_commit.tree()?.id();

    let base_tree = match parent {
        Some(p) => repo.find_commit(p)?.tree()?.id(),
        None => empty_tree(repo)?,
    };
    let new_tree_oid = splice_tree(repo, base_tree, dest, origin_tree)?;
    let new_tree = repo.find_tree(new_tree_oid)?;

    let message = with_origin_trailer(origin_commit.message().unwrap_or(""), origin);

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

/// The last origin commit reflected in the monorepo, found by scanning the
/// first-parent chain of `base` for the most recent `CapyFun-Origin` trailer.
/// Returns `None` when nothing has been imported yet.
fn last_imported_origin(repo: &Repository, base: Option<Oid>) -> Result<Option<Oid>> {
    let mut cur = base;
    while let Some(c) = cur {
        let commit = repo.find_commit(c)?;
        if let Some(sha) = parse_origin_trailer(commit.message().unwrap_or("")) {
            let oid = Oid::from_str(&sha)
                .with_context(|| format!("commit {c} has malformed {ORIGIN_TRAILER}: {sha}"))?;
            return Ok(Some(oid));
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
    if stop.is_some() {
        anyhow::bail!(
            "origin tip {tip} no longer contains the last imported commit {} on its \
             first-parent chain (was history rewritten / force-pushed?)",
            stop.expect("checked")
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
    let last = last_imported_origin(repo, base)?;
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

#[cfg(test)]
mod tests;
