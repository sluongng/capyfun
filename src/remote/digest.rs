//! REAPI content addressing: SHA256 digests and the `Directory` Merkle tree.
//!
//! Mirrors the buck2 fork's `app/buck2_execute/src/directory.rs`
//! (`create_re_directory` / `directory_to_re_tree`): a `Directory` lists its
//! `files` / `directories` / `symlinks` **sorted by name** for determinism; a
//! file is a `FileNode { digest, is_executable }`; a subdirectory is a
//! `DirectoryNode` carrying the SHA256 of *its own* serialized `Directory`. The
//! digest of any blob (file content or a serialized `Directory`) is
//! `sha256(bytes)` paired with its byte length.
//!
//! REAPI digests are **SHA256** (BuildBuddy's default instance), deliberately
//! separate from CapyFun's internal blake3 content store.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use prost::Message;
use sha2::{Digest as _, Sha256};

use super::proto::reapi;

/// A content-addressed blob (file content or a serialized `Directory`) ready to
/// upload to the CAS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub digest: reapi::Digest,
    pub data: Vec<u8>,
}

/// The SHA256 REAPI [`Digest`](reapi::Digest) of `data`: lowercase-hex hash plus
/// byte length.
pub fn sha256_digest(data: &[u8]) -> reapi::Digest {
    let hash = Sha256::digest(data);
    reapi::Digest {
        hash: hex_lower(&hash),
        size_bytes: data.len() as i64,
    }
}

/// Digest a prost message by its canonical encoding, returning both the digest
/// and the encoded bytes (the bytes are what gets uploaded to the CAS).
pub fn message_digest<M: Message>(m: &M) -> (reapi::Digest, Vec<u8>) {
    let data = m.encode_to_vec();
    let digest = sha256_digest(&data);
    (digest, data)
}

/// The result of serializing a filesystem tree into a REAPI input root: the root
/// `Directory` digest and every blob (file contents + each `Directory` proto)
/// that must exist in the CAS for the root to resolve.
#[derive(Debug, Clone)]
pub struct InputRoot {
    pub root: reapi::Digest,
    /// Deduplicated by digest hash, so identical files/dirs upload once.
    pub blobs: BTreeMap<String, Blob>,
}

/// Build the REAPI Merkle tree for the directory at `path`, content-addressing
/// every file and subdirectory. Deterministic: the same tree always yields the
/// same root digest regardless of on-disk iteration order.
///
/// Symlinks become `SymlinkNode`s (their target, not their contents); the
/// executable bit is taken from the owner-execute permission, matching how buck2
/// sets `FileNode.is_executable`.
pub fn build_input_root(path: &Path) -> Result<InputRoot> {
    let mut blobs = BTreeMap::new();
    let root = build_dir(path, &mut blobs)
        .with_context(|| format!("building REAPI input root from {}", path.display()))?;
    Ok(InputRoot { root, blobs })
}

fn build_dir(path: &Path, blobs: &mut BTreeMap<String, Blob>) -> Result<reapi::Digest> {
    let mut files: Vec<reapi::FileNode> = Vec::new();
    let mut directories: Vec<reapi::DirectoryNode> = Vec::new();
    let mut symlinks: Vec<reapi::SymlinkNode> = Vec::new();

    for entry in std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|n| anyhow::anyhow!("non-UTF-8 filename: {n:?}"))?;
        // `symlink_metadata` does not follow the link, so a symlink is reported
        // as a symlink rather than its target's type.
        let meta = entry.path().symlink_metadata()?;
        let ft = meta.file_type();

        if ft.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            symlinks.push(reapi::SymlinkNode {
                name,
                target: target.to_string_lossy().into_owned(),
                ..Default::default()
            });
        } else if ft.is_dir() {
            let digest = build_dir(&entry.path(), blobs)?;
            directories.push(reapi::DirectoryNode {
                name,
                digest: Some(digest),
            });
        } else {
            let data = std::fs::read(entry.path())?;
            let digest = sha256_digest(&data);
            let is_executable = meta.permissions().mode() & 0o100 != 0;
            insert_blob(blobs, &digest, data);
            files.push(reapi::FileNode {
                name,
                digest: Some(digest),
                is_executable,
                ..Default::default()
            });
        }
    }

    // Sort by name for a canonical, content-addressable serialization.
    files.sort_by(|a, b| a.name.cmp(&b.name));
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    symlinks.sort_by(|a, b| a.name.cmp(&b.name));

    let dir = reapi::Directory {
        files,
        directories,
        symlinks,
        ..Default::default()
    };
    let (digest, data) = message_digest(&dir);
    insert_blob(blobs, &digest, data);
    Ok(digest)
}

fn insert_blob(blobs: &mut BTreeMap<String, Blob>, digest: &reapi::Digest, data: Vec<u8>) {
    blobs
        .entry(digest.hash.clone())
        .or_insert_with(|| Blob {
            digest: digest.clone(),
            data,
        });
}

/// Assemble a REAPI [`Tree`](reapi::Tree) (root `Directory` + all descendant
/// `Directory`s) from a `root` digest and a blob set produced by
/// [`build_input_root`]. The inverse direction of building the input root, used
/// to package an output tree and (in tests) to round-trip materialization.
pub fn tree_from_blobs(root: &reapi::Digest, blobs: &BTreeMap<String, Blob>) -> Result<reapi::Tree> {
    let root_dir = decode_directory(root, blobs)?;
    let mut children = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect_descendants(&root_dir, blobs, &mut children, &mut seen)?;
    Ok(reapi::Tree {
        root: Some(root_dir),
        children,
    })
}

fn decode_directory(digest: &reapi::Digest, blobs: &BTreeMap<String, Blob>) -> Result<reapi::Directory> {
    let blob = blobs
        .get(&digest.hash)
        .with_context(|| format!("missing Directory blob {}", digest.hash))?;
    reapi::Directory::decode(blob.data.as_slice()).context("decoding Directory")
}

fn collect_descendants(
    dir: &reapi::Directory,
    blobs: &BTreeMap<String, Blob>,
    out: &mut Vec<reapi::Directory>,
    seen: &mut std::collections::HashSet<String>,
) -> Result<()> {
    for node in &dir.directories {
        let Some(d) = &node.digest else { continue };
        if !seen.insert(d.hash.clone()) {
            continue;
        }
        let child = decode_directory(d, blobs)?;
        collect_descendants(&child, blobs, out, seen)?;
        out.push(child);
    }
    Ok(())
}

/// Write a REAPI [`Tree`](reapi::Tree) to `dest` on disk, fetching file contents
/// via `fetch` (a batched blob reader: given digests, return their blobs). File
/// executable bits and symlinks are restored. This is how a remote action's
/// output is materialized back into the agent workdir.
pub fn materialize_tree<F>(tree: &reapi::Tree, dest: &Path, mut fetch: F) -> Result<()>
where
    F: FnMut(&[reapi::Digest]) -> Result<Vec<Blob>>,
{
    let root = tree.root.as_ref().context("output Tree has no root")?;
    // Child Directories are stored without their own digest, so index them by the
    // digest of their canonical serialization (matching the DirectoryNode refs).
    let mut dirs: HashMap<String, &reapi::Directory> = HashMap::new();
    for child in &tree.children {
        dirs.insert(message_digest(child).0.hash, child);
    }

    // Walk the tree, creating dirs/symlinks and collecting the files to fetch.
    let mut files: Vec<(PathBuf, reapi::FileNode)> = Vec::new();
    write_dir_structure(root, dest, &dirs, &mut files)?;

    // Batch-fetch every distinct file blob, then write each file.
    let mut wanted: Vec<reapi::Digest> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_, node) in &files {
        if let Some(d) = &node.digest {
            if seen.insert(d.hash.clone()) {
                wanted.push(d.clone());
            }
        }
    }
    let fetched = fetch(&wanted)?;
    let by_hash: HashMap<String, Vec<u8>> =
        fetched.into_iter().map(|b| (b.digest.hash, b.data)).collect();

    for (path, node) in files {
        let hash = node.digest.as_ref().map(|d| d.hash.as_str()).unwrap_or("");
        let data = by_hash
            .get(hash)
            .with_context(|| format!("output blob {hash} not returned by CAS"))?;
        std::fs::write(&path, data).with_context(|| format!("writing {}", path.display()))?;
        if node.is_executable {
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)?;
        }
    }
    Ok(())
}

fn write_dir_structure(
    dir: &reapi::Directory,
    dest: &Path,
    dirs: &HashMap<String, &reapi::Directory>,
    files: &mut Vec<(PathBuf, reapi::FileNode)>,
) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for f in &dir.files {
        files.push((dest.join(&f.name), f.clone()));
    }
    for s in &dir.symlinks {
        let link = dest.join(&s.name);
        // Best-effort: a re-run may find the link already present.
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&s.target, &link)
            .with_context(|| format!("symlinking {}", link.display()))?;
    }
    for node in &dir.directories {
        let d = node.digest.as_ref().context("DirectoryNode without digest")?;
        let child = dirs
            .get(&d.hash)
            .with_context(|| format!("output Tree missing child dir {}", d.hash))?;
        write_dir_structure(child, &dest.join(&node.name), dirs, files)?;
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests;
