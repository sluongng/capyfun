//! Generative-transform execution for the engine's tip layer.
//!
//! This module is the engine-facing seam for `agent_transform` execution. It
//! deliberately does **not** depend on [`crate::ir`]: the CLI resolves an IR
//! `AgentTransform` (agent label → harness/model, prompt-template file, vars)
//! into a fully-resolved [`AgentInvocation`] and hands the engine a ready-to-run
//! struct plus an [`AgentRunner`]. The engine renders nothing about config; it
//! materializes the agent's edits into a content-addressed patch and replays
//! that patch deterministically (see `docs/design/transformations.md`,
//! "Materialization and the content-addressed cache").

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::agent::{harness_command, resolve_credential, HarnessKind};

/// A fully-resolved agent run for one `agent_transform`, ready for the engine.
///
/// All *config* resolution (agent label → harness kind + model, prompt-template
/// file read, user-`vars` substitution) has already happened in the caller, so
/// the engine stays decoupled from [`crate::ir`]. The `prompt` here is the
/// template text with the user vars already substituted; the engine completes it
/// with the engine-derived typed context vars (`{{origin_commit}}`,
/// `{{origin_message}}`, `{{changed_files}}`, `{{incoming_diff}}`,
/// `{{repo_context}}`) at execution time via [`render_prompt`], because those
/// depend on the mirror state the engine owns. `agent_id` is the agent's label,
/// recorded in the `CapyFun-Agent` trailer.
#[derive(Debug, Clone)]
pub struct AgentInvocation {
    /// The harness runtime to drive (Claude Code, Codex, …).
    pub harness: HarnessKind,
    /// Model provider, for credential resolution and cache identity.
    pub provider: String,
    /// Provider-specific model id (passed to the harness, part of cache identity).
    pub model_id: String,
    /// Credential *reference* (e.g. `env:NAME`); resolved at execution time.
    pub credential: Option<String>,
    /// OpenAI-compatible base URL override (the `pi` HTTP harness).
    pub base_url: Option<String>,
    /// Prompt template text with user `vars` already substituted; the engine
    /// fills the remaining typed context vars at execution time.
    pub prompt: String,
    /// The agent rule's label, recorded in the `CapyFun-Agent` trailer.
    pub agent_id: String,
    /// Optional subtree-relative globs scoping the agent's view (informational;
    /// the harness sees the whole checked-out subtree). Empty = whole subtree.
    pub paths: Vec<String>,
}

/// The engine-derived context handed to [`render_prompt`] to complete a prompt.
///
/// These are the typed context vars from `docs/design/transformations.md` that
/// depend on the import state (the newest mirrored commit and the subtree). Each
/// fills a `{{var}}` token; missing tokens are left untouched so a template that
/// references none of them is unaffected.
#[derive(Debug, Default, Clone)]
pub struct PromptContext {
    /// `{{origin_commit}}`: the newest mirrored origin SHA (best-effort).
    pub origin_commit: String,
    /// `{{origin_message}}`: that origin commit's message (best-effort).
    pub origin_message: String,
    /// `{{changed_files}}`: paths the agent may touch — the subtree file list.
    pub changed_files: String,
    /// `{{incoming_diff}}`: the newest mirror commit's diff (best-effort).
    pub incoming_diff: String,
    /// `{{repo_context}}`: the destination subtree's file list (best-effort).
    pub repo_context: String,
}

/// Substitute the typed context `{{var}}` tokens into `prompt`. Unknown tokens
/// are left intact (a template need not reference every var). Pure given inputs.
pub fn render_prompt(prompt: &str, ctx: &PromptContext) -> String {
    prompt
        .replace("{{origin_commit}}", &ctx.origin_commit)
        .replace("{{origin_message}}", &ctx.origin_message)
        .replace("{{changed_files}}", &ctx.changed_files)
        .replace("{{incoming_diff}}", &ctx.incoming_diff)
        .replace("{{repo_context}}", &ctx.repo_context)
}

impl AgentInvocation {
    /// The cache-identity component of the agent: `(harness kind, provider, id)`.
    ///
    /// Per the doc, the credential reference is deliberately **not** part of the
    /// identity, so rotating a key does not invalidate materialized output.
    /// (Plugins/skills digests are part of identity in the full spec; the
    /// current `HarnessKind` carries no runfiles, so they are not yet folded in
    /// — see the cache-key note in `engine.rs`.)
    pub fn identity(&self) -> String {
        format!(
            "{}\0{}\0{}",
            self.harness.as_str(),
            self.provider,
            self.model_id
        )
    }
}

/// Runs an agent in a working directory, letting it edit files in place.
///
/// Abstracting this behind a trait keeps the materialize → diff → cache → commit
/// loop testable with a deterministic fake runner (no network / no live LLM).
/// The engine renders the final prompt (folding in its typed context vars) and
/// passes it as `prompt`, so the runner never re-renders.
pub trait AgentRunner {
    /// Run the agent for `inv` with the fully-rendered `prompt`, letting it edit
    /// files under `workdir` in place. The implementation must not move/create
    /// anything outside `workdir`.
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()>;
}

/// Production runner: shells out to the harness CLI with `workdir` as the
/// process CWD and the rendered prompt as the request, reusing the credential
/// resolution from [`crate::agent`]. For `claude_code`/`codex`/`antigravity`
/// this is the live path — running the harness in that CWD lets it edit files.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveRunner;

impl AgentRunner for LiveRunner {
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()> {
        // Resolve the credential the same way the standalone `agent-run` path
        // does, so a missing key still falls through to an ambient CLI login.
        // (The resolved variable is, by construction, already in our own env, so
        // the spawned process inherits it.)
        let _credential = resolve_credential(
            &inv.provider,
            inv.harness,
            inv.credential.as_deref(),
            |v| std::env::var(v).ok(),
        )
        .with_context(|| format!("resolving credential for agent {}", inv.agent_id))?;

        // The `pi` HTTP harness cannot edit files in a workdir (it only returns
        // text), so it is not a valid in-place editing runner.
        if inv.harness == HarnessKind::Pi {
            anyhow::bail!(
                "agent {} uses the `pi` harness, which returns text and cannot edit \
                 files in place; use a CLI harness (claude_code/codex/antigravity) \
                 for agent_transform execution",
                inv.agent_id
            );
        }

        let (program, args) = harness_command(inv.harness, Some(&inv.model_id), prompt)?;

        // Spawn the harness with `workdir` as CWD so it edits files in place.
        let output = std::process::Command::new(&program)
            .args(&args)
            .current_dir(workdir)
            .output()
            .with_context(|| format!("spawning harness `{program}` for agent {}", inv.agent_id))?;
        if !output.status.success() {
            anyhow::bail!(
                "agent {} harness `{program}` exited with {}: {}",
                inv.agent_id,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

/// Runs an agent on a REAPI backend (BuildBuddy) instead of locally.
///
/// Mirrors [`LiveRunner`] but dispatches the harness invocation to the remote
/// executor: the workdir subtree becomes the Action input root, the harness runs
/// on a worker, and the action's output subtree is materialized back into
/// `workdir` — so the engine's diff→materialize→cache loop is unchanged. The
/// Action Cache serves repeat runs (see [`crate::remote::executor`]).
///
/// Borrows the [`Reapi`](crate::remote::executor::Reapi) client so a fake can be
/// injected in tests; production passes a
/// [`RemoteClient`](crate::remote::client::RemoteClient).
pub struct RemoteRunner<'a> {
    client: &'a dyn crate::remote::executor::Reapi,
    /// Platform properties selecting the executor (container image, OS, network).
    platform: Vec<(String, String)>,
}

impl<'a> RemoteRunner<'a> {
    pub fn new(client: &'a dyn crate::remote::executor::Reapi) -> Self {
        RemoteRunner {
            client,
            platform: Vec::new(),
        }
    }

    pub fn with_platform(mut self, platform: Vec<(String, String)>) -> Self {
        self.platform = platform;
        self
    }
}

impl AgentRunner for RemoteRunner<'_> {
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()> {
        if inv.harness == HarnessKind::Pi {
            anyhow::bail!(
                "agent {} uses the `pi` harness, which cannot edit files; use a CLI \
                 harness for remote agent_transform execution",
                inv.agent_id
            );
        }
        // Build the same harness invocation the local runner would, then run it
        // remotely. The harness binary must exist in the executor image; the
        // model credential is supplied to the worker out-of-band (a BuildBuddy
        // secret), never as an env var, so it stays out of the Action digest.
        let (program, args) = harness_command(inv.harness, Some(&inv.model_id), prompt)?;
        let mut harness_args = Vec::with_capacity(args.len() + 1);
        harness_args.push(program);
        harness_args.extend(args);

        let executor = crate::remote::executor::RemoteExecutor::new(self.client)
            .with_platform(self.platform.clone());
        executor
            .run_in_place(workdir, harness_args, Vec::new(), false)
            .with_context(|| format!("remote agent {} run", inv.agent_id))?;
        Ok(())
    }
}

/// A deterministic, hermetic [`AgentRunner`] for demos, evals, and tests.
///
/// Instead of calling a real model, it runs a recorded shell script that makes
/// the *same* edits a coding agent would — so the full materialize → diff →
/// cache → replay loop runs with **no model access, no network, and zero cost**,
/// yet exercises the exact production code path (the engine cannot tell a fixture
/// runner from a live one). Each agent's script is `<dir>/<name>.sh`, where
/// `<name>` is the agent label's target (`//tools/agent:porter` → `porter`).
///
/// The script runs with the checked-out subtree as its CWD and sees the rendered
/// prompt in `$CAPYFUN_AGENT_PROMPT`, so a fixture can branch on the verifier
/// feedback [`VerifyingRunner`] appends on a retry. This is a **mock** by
/// construction — callers label its output as such (e.g. `fixture/mock`).
pub struct FixtureRunner {
    dir: PathBuf,
}

impl FixtureRunner {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FixtureRunner { dir: dir.into() }
    }

    /// The fixture script stem for an agent label: the part after the last `:` or
    /// `/`, e.g. `//tools/agent:porter` → `porter`.
    fn script_name(agent_id: &str) -> &str {
        agent_id.rsplit([':', '/']).next().unwrap_or(agent_id)
    }
}

impl AgentRunner for FixtureRunner {
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()> {
        let name = Self::script_name(&inv.agent_id);
        let script = self.dir.join(format!("{name}.sh"));
        if !script.exists() {
            anyhow::bail!(
                "fixture agent script for {} not found: expected {}",
                inv.agent_id,
                script.display()
            );
        }
        let output = Command::new("sh")
            .arg(&script)
            .current_dir(workdir)
            .env("CAPYFUN_AGENT_PROMPT", prompt)
            .env("CAPYFUN_AGENT_ID", &inv.agent_id)
            .output()
            .with_context(|| format!("running fixture agent script {}", script.display()))?;
        if !output.status.success() {
            anyhow::bail!(
                "fixture agent {} script {} exited with {}: {}",
                inv.agent_id,
                script.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

/// Wraps another [`AgentRunner`] with a **verify → retry** loop.
///
/// After the agent edits the subtree, a verifier command runs in the same working
/// directory; on failure its combined output is fed back to the agent (appended
/// to the prompt) and the agent runs again, up to `max_retries` times. This is
/// the "agent edits → verifier runs → on failure feed stderr+diff back → retry →
/// materialize" loop from `docs/design/transformations.md`, made real.
///
/// The verifier runs via `sh -c <cmd>`, so it can be any shell command (e.g.
/// `go test ./...`). Because the engine computes the cache key from the
/// *original* rendered prompt before calling the runner, the retry feedback never
/// changes cache identity: the materialized patch is the final, **verified**
/// state, and replays are still free.
pub struct VerifyingRunner<'a> {
    inner: &'a dyn AgentRunner,
    verify: String,
    max_retries: usize,
}

impl<'a> VerifyingRunner<'a> {
    pub fn new(inner: &'a dyn AgentRunner, verify: impl Into<String>, max_retries: usize) -> Self {
        VerifyingRunner {
            inner,
            verify: verify.into(),
            max_retries,
        }
    }

    /// Run the configured verifier in `workdir`, returning `(ok, combined output)`.
    fn verify_in(&self, workdir: &Path) -> Result<(bool, String)> {
        let out = Command::new("sh")
            .arg("-c")
            .arg(&self.verify)
            .current_dir(workdir)
            .output()
            .with_context(|| format!("running verifier `{}`", self.verify))?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout).trim_end(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        );
        Ok((out.status.success(), combined.trim().to_owned()))
    }
}

impl AgentRunner for VerifyingRunner<'_> {
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()> {
        let mut current = prompt.to_owned();
        for attempt in 0..=self.max_retries {
            self.inner.run(inv, &current, workdir)?;
            let (ok, output) = self.verify_in(workdir)?;
            if ok {
                if attempt > 0 {
                    eprintln!("agent {}: verifier passed on retry {attempt}", inv.agent_id);
                }
                return Ok(());
            }
            if attempt == self.max_retries {
                anyhow::bail!(
                    "agent {}: verifier `{}` still failing after {} attempt(s):\n{output}",
                    inv.agent_id,
                    self.verify,
                    attempt + 1
                );
            }
            eprintln!(
                "agent {}: verifier failed (attempt {}); feeding output back and retrying",
                inv.agent_id,
                attempt + 1
            );
            current = format!(
                "{prompt}\n\n--- VERIFIER FAILED (attempt {}) ---\n{output}\n\
                 Fix the code so the verifier (`{}`) passes.",
                attempt + 1,
                self.verify
            );
        }
        unreachable!("the loop returns or bails on the last attempt")
    }
}

#[cfg(test)]
mod remote_runner_tests;

#[cfg(test)]
mod runner_tests;
