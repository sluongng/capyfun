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

use std::path::Path;

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
