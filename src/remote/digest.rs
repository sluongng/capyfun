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

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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
