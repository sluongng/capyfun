//! Tests for REAPI SHA256 digests and the `Directory` Merkle tree. All hermetic
//! (tempfile only); no network.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;

/// SHA256 of the empty input — the canonical NIST vector — proves the hash and
/// the lowercase-hex encoding.
#[test]
fn sha256_empty_vector() {
    let d = sha256_digest(b"");
    assert_eq!(
        d.hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(d.size_bytes, 0);
}

/// A known non-empty vector: SHA256("abc").
#[test]
fn sha256_abc_vector() {
    let d = sha256_digest(b"abc");
    assert_eq!(
        d.hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(d.size_bytes, 3);
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// A nested tree round-trips into blobs and the executable bit is captured.
#[test]
fn input_root_captures_files_and_exec_bit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("README.md"), "hello\n");
    write(&root.join("src/main.rs"), "fn main() {}\n");
    let script = root.join("run.sh");
    write(&script, "#!/bin/sh\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let ir = build_input_root(root).unwrap();

    // Every file's content is present as a blob, addressed by its own sha256.
    let readme = sha256_digest(b"hello\n");
    assert!(ir.blobs.contains_key(&readme.hash));
    assert_eq!(ir.blobs[&readme.hash].data, b"hello\n");

    // The root Directory blob exists and decodes; run.sh is marked executable,
    // README.md is not.
    let root_blob = &ir.blobs[&ir.root.hash];
    let dir = reapi::Directory::decode(root_blob.data.as_slice()).unwrap();
    let run = dir.files.iter().find(|f| f.name == "run.sh").unwrap();
    let readme_node = dir.files.iter().find(|f| f.name == "README.md").unwrap();
    assert!(run.is_executable);
    assert!(!readme_node.is_executable);
    // `src` is a subdirectory node, not a file.
    assert!(dir.directories.iter().any(|d| d.name == "src"));
}

/// Files appear sorted by name in the serialized `Directory`, so the root digest
/// is independent of filesystem iteration order.
#[test]
fn root_digest_is_deterministic_and_order_independent() {
    let a = tempfile::tempdir().unwrap();
    write(&a.path().join("b.txt"), "B");
    write(&a.path().join("a.txt"), "A");
    write(&a.path().join("c.txt"), "C");

    let b = tempfile::tempdir().unwrap();
    write(&b.path().join("c.txt"), "C");
    write(&b.path().join("a.txt"), "A");
    write(&b.path().join("b.txt"), "B");

    let ra = build_input_root(a.path()).unwrap();
    let rb = build_input_root(b.path()).unwrap();
    assert_eq!(ra.root, rb.root);

    // And the entries are actually sorted.
    let dir = reapi::Directory::decode(ra.blobs[&ra.root.hash].data.as_slice()).unwrap();
    let names: Vec<_> = dir.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);
}

/// Changing any file content changes the root digest (it is a Merkle root).
#[test]
fn content_change_changes_root() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("x"), "one");
    let before = build_input_root(tmp.path()).unwrap().root;
    write(&tmp.path().join("x"), "two");
    let after = build_input_root(tmp.path()).unwrap().root;
    assert_ne!(before.hash, after.hash);
}

/// Identical content under different names is stored once (blob dedup by digest).
#[test]
fn identical_blobs_dedup() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("one.txt"), "same");
    write(&tmp.path().join("two.txt"), "same");
    let ir = build_input_root(tmp.path()).unwrap();
    let same = sha256_digest(b"same");
    // One content blob for "same" (plus the single root Directory blob).
    assert_eq!(ir.blobs[&same.hash].data, b"same");
    let content_blobs = ir
        .blobs
        .values()
        .filter(|b| b.data == b"same")
        .count();
    assert_eq!(content_blobs, 1);
}

/// A built input root packages into a `Tree` and materializes back to an
/// identical directory (round-trip), including nested dirs and the exec bit.
#[test]
fn tree_roundtrip_materialization() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("README.md"), "hello\n");
    write(&src.path().join("src/main.rs"), "fn main() {}\n");
    write(&src.path().join("src/lib/util.rs"), "pub fn u() {}\n");
    let script = src.path().join("run.sh");
    write(&script, "#!/bin/sh\necho hi\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let ir = build_input_root(src.path()).unwrap();
    let tree = tree_from_blobs(&ir.root, &ir.blobs).unwrap();

    // Materialize using the in-memory blob set as the "CAS".
    let dest = tempfile::tempdir().unwrap();
    materialize_tree(&tree, dest.path(), |digests| {
        Ok(digests
            .iter()
            .map(|d| ir.blobs[&d.hash].clone())
            .collect())
    })
    .unwrap();

    // The materialized tree hashes to the same root digest.
    let round = build_input_root(dest.path()).unwrap();
    assert_eq!(round.root, ir.root);
    // And the exec bit survived.
    let mode = fs::metadata(dest.path().join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert!(mode & 0o100 != 0);
    assert_eq!(fs::read_to_string(dest.path().join("src/lib/util.rs")).unwrap(), "pub fn u() {}\n");
}

/// `tree_from_blobs` collects every descendant directory exactly once.
#[test]
fn tree_collects_all_descendants() {
    let src = tempfile::tempdir().unwrap();
    write(&src.path().join("a/b/c/deep.txt"), "deep");
    write(&src.path().join("a/sibling.txt"), "s");
    let ir = build_input_root(src.path()).unwrap();
    let tree = tree_from_blobs(&ir.root, &ir.blobs).unwrap();
    // root + a + a/b + a/b/c == 4 directories total; children excludes root → 3.
    assert_eq!(tree.children.len(), 3);
}

/// A symlink is recorded as a `SymlinkNode` pointing at its target, not followed.
#[test]
fn symlink_recorded_as_node() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("real.txt"), "data");
    std::os::unix::fs::symlink("real.txt", tmp.path().join("link.txt")).unwrap();
    let ir = build_input_root(tmp.path()).unwrap();
    let dir = reapi::Directory::decode(ir.blobs[&ir.root.hash].data.as_slice()).unwrap();
    let link = dir.symlinks.iter().find(|s| s.name == "link.txt").unwrap();
    assert_eq!(link.target, "real.txt");
}
