//! Tests for [`RemoteRunner`]: it builds the right harness invocation and
//! materializes the remote action's output back into the workdir, all driven
//! through the [`AgentRunner`] trait with an in-memory fake REAPI backend.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use prost::Message;

use super::*;
use crate::remote::digest::{build_input_root, message_digest, tree_from_blobs, Blob};
use crate::remote::executor::Reapi;
use crate::remote::proto::reapi;

/// Minimal in-memory CAS/AC that records uploaded blobs and, on execute, returns
/// `output_root` packaged as the action's output tree.
struct Fake {
    cas: RefCell<HashMap<String, Vec<u8>>>,
    uploaded: RefCell<Vec<Blob>>,
    output_root: PathBuf,
}

impl Fake {
    fn new(output_root: PathBuf) -> Self {
        Fake {
            cas: RefCell::new(HashMap::new()),
            uploaded: RefCell::new(Vec::new()),
            output_root,
        }
    }
    fn package(&self) -> reapi::ActionResult {
        let ir = build_input_root(&self.output_root).unwrap();
        let tree = tree_from_blobs(&ir.root, &ir.blobs).unwrap();
        let (td, tb) = message_digest(&tree);
        let mut cas = self.cas.borrow_mut();
        for b in ir.blobs.values() {
            cas.insert(b.digest.hash.clone(), b.data.clone());
        }
        cas.insert(td.hash.clone(), tb);
        reapi::ActionResult {
            output_directories: vec![reapi::OutputDirectory {
                path: "w".to_owned(),
                tree_digest: Some(td),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}

impl Reapi for Fake {
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
            self.uploaded.borrow_mut().push(b.clone());
        }
        Ok(())
    }
    fn batch_read(&self, digests: Vec<reapi::Digest>) -> Result<Vec<Blob>> {
        let cas = self.cas.borrow();
        digests
            .into_iter()
            .map(|d| {
                let data = cas.get(&d.hash).cloned().context("missing blob")?;
                Ok(Blob { digest: d, data })
            })
            .collect()
    }
    fn get_action_result(&self, _: reapi::Digest) -> Result<Option<reapi::ActionResult>> {
        Ok(None)
    }
    fn execute(&self, _: reapi::Digest, _: bool) -> Result<reapi::ExecuteResponse> {
        Ok(reapi::ExecuteResponse {
            result: Some(self.package()),
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

fn invocation(harness: HarnessKind) -> AgentInvocation {
    AgentInvocation {
        harness,
        provider: "anthropic".to_owned(),
        model_id: "claude-opus-4-8".to_owned(),
        credential: None,
        base_url: None,
        prompt: "modernize this".to_owned(),
        agent_id: "//tools/agent:modernize".to_owned(),
        paths: Vec::new(),
    }
}

#[test]
fn remote_runner_materializes_output() {
    let input = tempfile::tempdir().unwrap();
    write(&input.path().join("a.txt"), "old\n");

    let output = tempfile::tempdir().unwrap();
    write(&output.path().join("a.txt"), "new\n");

    let fake = Fake::new(output.path().to_path_buf());
    let runner = RemoteRunner::new(&fake);

    runner
        .run(&invocation(HarnessKind::ClaudeCode), "modernize this", input.path())
        .unwrap();

    assert_eq!(fs::read_to_string(input.path().join("a.txt")).unwrap(), "new\n");
}

#[test]
fn remote_runner_builds_wrapped_harness_command() {
    let input = tempfile::tempdir().unwrap();
    write(&input.path().join("a.txt"), "x\n");
    let output = tempfile::tempdir().unwrap();
    write(&output.path().join("a.txt"), "x\n");

    let fake = Fake::new(output.path().to_path_buf());
    let runner = RemoteRunner::new(&fake);
    runner
        .run(&invocation(HarnessKind::ClaudeCode), "modernize this", input.path())
        .unwrap();

    // Find the uploaded Command and check it runs claude under the cd-wrapper.
    let cmd = fake
        .uploaded
        .borrow()
        .iter()
        .filter_map(|b| reapi::Command::decode(b.data.as_slice()).ok())
        .find(|c| c.arguments.iter().any(|a| a == "claude"))
        .expect("a Command referencing claude was uploaded");

    assert_eq!(cmd.arguments[0], "sh");
    assert_eq!(cmd.arguments[1], "-c");
    assert!(cmd.arguments[2].contains("cd w &&"));
    assert!(cmd.arguments.iter().any(|a| a == "-p"));
    assert!(cmd.arguments.iter().any(|a| a == "modernize this"));
    assert!(cmd.arguments.iter().any(|a| a == "--model"));
    assert!(cmd.arguments.iter().any(|a| a == "claude-opus-4-8"));
    // The captured output path is the wrapped work dir.
    assert_eq!(cmd.output_paths, ["w"]);
}

#[test]
fn remote_runner_rejects_pi() {
    let input = tempfile::tempdir().unwrap();
    write(&input.path().join("a.txt"), "x\n");
    let output = tempfile::tempdir().unwrap();
    let fake = Fake::new(output.path().to_path_buf());
    let runner = RemoteRunner::new(&fake);
    let err = runner
        .run(&invocation(HarnessKind::Pi), "p", input.path())
        .unwrap_err();
    assert!(err.to_string().contains("pi"));
}
