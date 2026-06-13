//! High-level remote execution of a generative transform.
//!
//! [`RemoteExecutor::run_in_place`] takes an agent workdir (a checked-out
//! subtree), runs the harness as a REAPI Action on the server, and materializes
//! the action's output subtree back into the workdir — so it satisfies the
//! [`AgentRunner`](crate::engine::AgentRunner) contract (leave edits in the
//! workdir) without the engine knowing whether the agent ran locally or on RBE.
//!
//! The **Action Cache is the agent-output cache** (R3): before executing, the
//! executor checks `GetActionResult(action_digest)`; a hit returns the cached
//! output tree and the harness never runs. The Action digest already captures
//! the subtree, the harness invocation, and the platform (see
//! [`super::action`]), so the cache key needs nothing extra.
//!
//! REAPI calls go through the [`Reapi`] trait so the orchestration is exercised
//! hermetically by a fake in tests; [`super::client::RemoteClient`] is the live
//! implementation.

use std::path::Path;

use anyhow::{bail, Context, Result};
use prost::Message;

use super::action::{build_action, ActionSpec};
use super::client::RemoteClient;
use super::digest::{build_input_root, materialize_tree, message_digest, Blob};
use super::proto::reapi;

/// The input-root subdirectory the subtree is nested under, so the action can
/// declare it as a captured output path (`output_paths` are relative to the
/// working directory, which is the input root here).
const WORK: &str = "w";

/// The REAPI operations the executor needs. Implemented by
/// [`RemoteClient`](super::client::RemoteClient); faked in tests.
pub trait Reapi {
    fn find_missing(&self, digests: Vec<reapi::Digest>) -> Result<Vec<reapi::Digest>>;
    fn batch_update(&self, blobs: &[Blob]) -> Result<()>;
    fn batch_read(&self, digests: Vec<reapi::Digest>) -> Result<Vec<Blob>>;
    fn get_action_result(&self, action: reapi::Digest) -> Result<Option<reapi::ActionResult>>;
    fn execute(&self, action: reapi::Digest, skip_cache_lookup: bool) -> Result<reapi::ExecuteResponse>;

    /// Upload only the blobs the CAS is missing. Provided in terms of the two
    /// primitives so fakes get it for free.
    fn upload_missing(&self, blobs: &[Blob]) -> Result<usize> {
        let digests = blobs.iter().map(|b| b.digest.clone()).collect();
        let missing = self.find_missing(digests)?;
        if missing.is_empty() {
            return Ok(0);
        }
        let want: std::collections::HashSet<_> = missing.into_iter().map(|d| d.hash).collect();
        let to_upload: Vec<Blob> = blobs
            .iter()
            .filter(|b| want.contains(&b.digest.hash))
            .cloned()
            .collect();
        let n = to_upload.len();
        self.batch_update(&to_upload)?;
        Ok(n)
    }
}

impl Reapi for RemoteClient {
    fn find_missing(&self, digests: Vec<reapi::Digest>) -> Result<Vec<reapi::Digest>> {
        self.find_missing_blobs(digests)
    }
    fn batch_update(&self, blobs: &[Blob]) -> Result<()> {
        self.batch_update_blobs(blobs)
    }
    fn batch_read(&self, digests: Vec<reapi::Digest>) -> Result<Vec<Blob>> {
        self.batch_read_blobs(digests)
    }
    fn get_action_result(&self, action: reapi::Digest) -> Result<Option<reapi::ActionResult>> {
        RemoteClient::get_action_result(self, action)
    }
    fn execute(&self, action: reapi::Digest, skip: bool) -> Result<reapi::ExecuteResponse> {
        RemoteClient::execute(self, action, skip)
    }
}

/// What happened on a remote run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteOutcome {
    /// The action's process exit code.
    pub exit_code: i32,
    /// Whether the result was served from the Action Cache (no harness run).
    pub cached: bool,
}

/// Runs generative transforms on a REAPI backend.
pub struct RemoteExecutor<'a> {
    client: &'a dyn Reapi,
    /// Platform properties selecting the executor (container image, OS, network).
    pub platform: Vec<(String, String)>,
    /// Optional execution timeout, in seconds.
    pub timeout_secs: Option<i64>,
}

impl<'a> RemoteExecutor<'a> {
    pub fn new(client: &'a dyn Reapi) -> Self {
        RemoteExecutor {
            client,
            platform: Vec::new(),
            timeout_secs: None,
        }
    }

    pub fn with_platform(mut self, platform: Vec<(String, String)>) -> Self {
        self.platform = platform;
        self
    }

    /// Run `harness_args` over the subtree at `workdir`, materializing the
    /// action's edited subtree back into `workdir` in place.
    ///
    /// `env` are non-secret environment variables (the model credential travels
    /// as a gRPC header, never here). With `no_cache`, the Action Cache is
    /// bypassed and the harness is forced to run.
    pub fn run_in_place(
        &self,
        workdir: &Path,
        harness_args: Vec<String>,
        env: Vec<(String, String)>,
        no_cache: bool,
    ) -> Result<RemoteOutcome> {
        // 1. Content-address the subtree and nest it under WORK so the action can
        //    name it as an output path.
        let inner = build_input_root(workdir).context("building input root")?;
        let (root_digest, wrapper) = wrap_under(&inner.root, WORK);

        // 2. Run the harness with cwd = WORK via a quoting-safe wrapper, capturing
        //    the whole WORK subtree as the output.
        let spec = ActionSpec {
            arguments: cd_and_exec(WORK, &harness_args),
            output_paths: vec![WORK.to_owned()],
            working_directory: String::new(),
            env,
            platform: self.platform.clone(),
            input_root: root_digest,
            timeout_secs: self.timeout_secs,
            do_not_cache: no_cache,
        };
        let built = build_action(&spec);

        // 3. Action Cache lookup (R3): a hit skips execution entirely.
        let (result, cached) = match if no_cache {
            None
        } else {
            self.client.get_action_result(built.action_digest.clone())?
        } {
            Some(r) => (r, true),
            None => {
                // Upload the input blobs (subtree + wrapper + command + action),
                // then execute.
                let mut blobs: Vec<Blob> = inner.blobs.values().cloned().collect();
                blobs.push(wrapper);
                blobs.extend(built.blobs.iter().cloned());
                self.client.upload_missing(&blobs).context("uploading inputs")?;

                let resp = self.client.execute(built.action_digest.clone(), no_cache)?;
                if let Some(status) = &resp.status {
                    if status.code != 0 {
                        bail!("remote execution error: code {} {}", status.code, status.message);
                    }
                }
                let r = resp.result.context("Execute returned no ActionResult")?;
                (r, resp.cached_result)
            }
        };

        if result.exit_code != 0 {
            bail!("remote harness exited with code {}", result.exit_code);
        }

        // 4. Materialize the output subtree back into the workdir.
        self.materialize_output(&result, workdir)?;

        Ok(RemoteOutcome {
            exit_code: result.exit_code,
            cached,
        })
    }

    fn materialize_output(&self, result: &reapi::ActionResult, workdir: &Path) -> Result<()> {
        let od = result
            .output_directories
            .iter()
            .find(|d| d.path == WORK)
            .with_context(|| format!("action produced no `{WORK}` output directory"))?;
        let tree_digest = od
            .tree_digest
            .clone()
            .context("OutputDirectory without tree_digest")?;
        let tree_blob = self
            .client
            .batch_read(vec![tree_digest])?
            .pop()
            .context("output Tree not in CAS")?;
        let tree = reapi::Tree::decode(tree_blob.data.as_slice()).context("decoding output Tree")?;

        // Replace the workdir contents with the action's output so deletions and
        // additions are reflected, not just edits.
        clear_dir(workdir)?;
        materialize_tree(&tree, workdir, |digests| {
            self.client.batch_read(digests.to_vec())
        })
    }
}

/// Wrap an inner input-root digest under a single named subdirectory, returning
/// the wrapper root digest and the wrapper `Directory` blob.
fn wrap_under(inner_root: &reapi::Digest, name: &str) -> (reapi::Digest, Blob) {
    let wrapper = reapi::Directory {
        directories: vec![reapi::DirectoryNode {
            name: name.to_owned(),
            digest: Some(inner_root.clone()),
        }],
        ..Default::default()
    };
    let (digest, data) = message_digest(&wrapper);
    (
        digest.clone(),
        Blob {
            digest,
            data,
        },
    )
}

/// Build a shell command that `cd`s into `dir` and execs `args` verbatim. Using
/// `sh -c 'cd <dir> && exec "$@"' sh <args...>` passes the harness argv through
/// `"$@"`, so no shell quoting of the harness arguments is needed.
fn cd_and_exec(dir: &str, args: &[String]) -> Vec<String> {
    let mut out = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!("cd {dir} && exec \"$@\""),
        "sh".to_owned(),
    ];
    out.extend(args.iter().cloned());
    out
}

/// Remove all entries inside `dir` (but not `dir` itself).
fn clear_dir(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
