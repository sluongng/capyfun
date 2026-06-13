//! Tests for the remote executor.
//!
//! The orchestration (input-root build, AC lookup, upload, execute, output
//! materialization) is exercised hermetically with an in-memory [`FakeReapi`]
//! that behaves like a CAS + Action Cache. The live end-to-end test against
//! BuildBuddy is gated on `BUILDBUDDY_API_KEY`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::remote::digest::{build_input_root, message_digest, tree_from_blobs};

/// In-memory CAS + Action Cache. On `execute`, it packages the contents of
/// `output_root` as the action's output tree (simulating a harness that
/// transformed the subtree into `output_root`).
struct FakeReapi {
    cas: RefCell<HashMap<String, Vec<u8>>>,
    ac: RefCell<HashMap<String, reapi::ActionResult>>,
    output_root: PathBuf,
    execute_calls: Cell<usize>,
}

impl FakeReapi {
    fn new(output_root: PathBuf) -> Self {
        FakeReapi {
            cas: RefCell::new(HashMap::new()),
            ac: RefCell::new(HashMap::new()),
            output_root,
            execute_calls: Cell::new(0),
        }
    }

    /// Package `output_root` as an ActionResult, storing its blobs + Tree in CAS.
    fn package_output(&self) -> reapi::ActionResult {
        let ir = build_input_root(&self.output_root).unwrap();
        let tree = tree_from_blobs(&ir.root, &ir.blobs).unwrap();
        let (tree_d, tree_bytes) = message_digest(&tree);
        {
            let mut cas = self.cas.borrow_mut();
            for b in ir.blobs.values() {
                cas.insert(b.digest.hash.clone(), b.data.clone());
            }
            cas.insert(tree_d.hash.clone(), tree_bytes);
        }
        reapi::ActionResult {
            output_directories: vec![reapi::OutputDirectory {
                path: "w".to_owned(),
                tree_digest: Some(tree_d),
                ..Default::default()
            }],
            exit_code: 0,
            ..Default::default()
        }
    }
}

impl Reapi for FakeReapi {
    fn find_missing(&self, digests: Vec<reapi::Digest>) -> Result<Vec<reapi::Digest>> {
        let cas = self.cas.borrow();
        Ok(digests
            .into_iter()
            .filter(|d| !cas.contains_key(&d.hash))
            .collect())
    }
    fn batch_update(&self, blobs: &[Blob]) -> Result<()> {
        let mut cas = self.cas.borrow_mut();
        for b in blobs {
            cas.insert(b.digest.hash.clone(), b.data.clone());
        }
        Ok(())
    }
    fn batch_read(&self, digests: Vec<reapi::Digest>) -> Result<Vec<Blob>> {
        let cas = self.cas.borrow();
        digests
            .into_iter()
            .map(|d| {
                let data = cas
                    .get(&d.hash)
                    .cloned()
                    .with_context(|| format!("fake CAS missing {}", d.hash))?;
                Ok(Blob { digest: d, data })
            })
            .collect()
    }
    fn get_action_result(&self, action: reapi::Digest) -> Result<Option<reapi::ActionResult>> {
        Ok(self.ac.borrow().get(&action.hash).cloned())
    }
    fn execute(&self, action: reapi::Digest, _skip: bool) -> Result<reapi::ExecuteResponse> {
        self.execute_calls.set(self.execute_calls.get() + 1);
        let ar = self.package_output();
        // The server populates the Action Cache on a successful execution.
        self.ac.borrow_mut().insert(action.hash, ar.clone());
        Ok(reapi::ExecuteResponse {
            result: Some(ar),
            cached_result: false,
            ..Default::default()
        })
    }
}

fn write(path: &Path, contents: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// Set up an input workdir and a distinct desired-output dir.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let input = tempfile::tempdir().unwrap();
    write(&input.path().join("a.txt"), "old\n");
    write(&input.path().join("keep.txt"), "k\n");

    let output = tempfile::tempdir().unwrap();
    write(&output.path().join("a.txt"), "new\n"); // edited
    write(&output.path().join("keep.txt"), "k\n"); // unchanged
    write(&output.path().join("added.rs"), "fn x() {}\n"); // added
    (input, output)
}

#[test]
fn run_materializes_output_into_workdir() {
    let (input, output) = fixture();
    let fake = FakeReapi::new(output.path().to_path_buf());
    let exec = RemoteExecutor::new(&fake);

    let outcome = exec
        .run_in_place(
            input.path(),
            vec!["sh".into(), "-c".into(), "true".into()],
            vec![],
            false,
        )
        .unwrap();

    assert!(!outcome.cached);
    assert_eq!(fake.execute_calls.get(), 1);
    // The workdir now reflects the remote output: edit, addition, unchanged file.
    assert_eq!(fs::read_to_string(input.path().join("a.txt")).unwrap(), "new\n");
    assert_eq!(fs::read_to_string(input.path().join("added.rs")).unwrap(), "fn x() {}\n");
    assert_eq!(fs::read_to_string(input.path().join("keep.txt")).unwrap(), "k\n");
    // The input blobs were uploaded.
    assert!(!fake.cas.borrow().is_empty());
}

#[test]
fn action_cache_hit_skips_execute() {
    let (input, output) = fixture();
    let fake = FakeReapi::new(output.path().to_path_buf());
    let exec = RemoteExecutor::new(&fake);

    // First run: cache miss → executes and populates the AC.
    let first = exec
        .run_in_place(input.path(), vec!["sh".into()], vec![], false)
        .unwrap();
    assert!(!first.cached);
    assert_eq!(fake.execute_calls.get(), 1);

    // Reset the workdir to the original input, then run again: the Action digest
    // is identical, so it is an AC hit — no second execute, output still applied.
    fs::remove_dir_all(input.path()).unwrap();
    fs::create_dir_all(input.path()).unwrap();
    write(&input.path().join("a.txt"), "old\n");
    write(&input.path().join("keep.txt"), "k\n");

    let second = exec
        .run_in_place(input.path(), vec!["sh".into()], vec![], false)
        .unwrap();
    assert!(second.cached, "second identical run should be an Action Cache hit");
    assert_eq!(fake.execute_calls.get(), 1, "execute must not run again on a hit");
    assert_eq!(fs::read_to_string(input.path().join("a.txt")).unwrap(), "new\n");
}

#[test]
fn no_cache_forces_execute_even_on_hit() {
    let (input, output) = fixture();
    let fake = FakeReapi::new(output.path().to_path_buf());
    let exec = RemoteExecutor::new(&fake);

    exec.run_in_place(input.path(), vec!["sh".into()], vec![], false)
        .unwrap();
    assert_eq!(fake.execute_calls.get(), 1);

    // no_cache bypasses the AC lookup and forces another execute.
    let again = exec
        .run_in_place(input.path(), vec!["sh".into()], vec![], true)
        .unwrap();
    assert!(!again.cached);
    assert_eq!(fake.execute_calls.get(), 2);
}

/// Live remote execution against BuildBuddy: run a tiny `sh` action that edits
/// the subtree, confirm the edit is materialized back, and that a second run is
/// served from the Action Cache. Skipped unless `BUILDBUDDY_API_KEY` is set.
#[test]
fn live_remote_execution_and_cache() {
    use crate::remote::client::{RemoteClient, RemoteConfig};

    let cfg = RemoteConfig::from_env();
    if cfg.api_key.is_none() {
        eprintln!("skipping: BUILDBUDDY_API_KEY not set");
        return;
    }
    let image = std::env::var("BUILDBUDDY_EXEC_IMAGE")
        .unwrap_or_else(|_| "docker://mirror.gcr.io/library/busybox:latest".to_owned());

    let client = RemoteClient::connect(&cfg).expect("connect");
    let exec = RemoteExecutor::new(&client).with_platform(vec![
        ("OSFamily".to_owned(), "Linux".to_owned()),
        ("container-image".to_owned(), image),
    ]);

    let workdir = tempfile::tempdir().unwrap();
    write(&workdir.path().join("marker.txt"), "base\n");
    let args = vec!["sh".into(), "-c".into(), "echo remote-edit >> marker.txt".into()];

    // First run (caching on so the result is stored; the AC may already be warm
    // from a prior run of this deterministic action — either is fine). The edit
    // must be materialized back into the workdir.
    exec.run_in_place(workdir.path(), args.clone(), vec![], false)
        .expect("remote run");
    let got = fs::read_to_string(workdir.path().join("marker.txt")).unwrap();
    assert!(got.contains("remote-edit"), "edit should be materialized: {got:?}");

    // Reset and run the identical action again: it must now be an Action Cache
    // hit, and the cached output is materialized just the same.
    write(&workdir.path().join("marker.txt"), "base\n");
    let second = exec
        .run_in_place(workdir.path(), args, vec![], false)
        .expect("remote run 2");
    assert!(second.cached, "second identical run should be an Action Cache hit");
    assert!(fs::read_to_string(workdir.path().join("marker.txt"))
        .unwrap()
        .contains("remote-edit"));
}
