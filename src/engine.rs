//! Git rewrite/projection engine.
//!
//! Houses the JOSH-shaped tree-prefix primitive and (in later milestones) the
//! commit replay logic for import. This is the only module that performs Git
//! I/O.

use anyhow::{Context, Result};
use git2::{Oid, Repository, Tree};

/// Git filemode for a tree (subdirectory) entry.
const FILEMODE_TREE: i32 = 0o040000;

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

#[cfg(test)]
mod tests;
