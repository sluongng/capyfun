//! Issue-triggered reactions: run a coding agent over a repo and open a PR.
//!
//! This is the generative counterpart to import/export. Where the reconciler
//! reacts to upstream *pushes* by moving code across a tree boundary, a
//! [`Reaction`](crate::ir::Reaction) reacts to a GitHub **issue** by running an
//! `agent` over a checkout of the repo and opening a pull request with its
//! prototype. See `docs/design/reactions.md`.
//!
//! The flow, given a matched issue event:
//!
//! 1. clone the repo (auth + URL from the [`Forge`]) into a temp checkout;
//! 2. branch `capyfun/issue-<n>`;
//! 3. run the agent in that checkout (the [`AgentRunner`] edits files in place);
//! 4. if it made changes, commit them with `CapyFun-Agent`/`CapyFun-Issue`
//!    trailers, push the branch, and open a PR (via the [`Forge`]).
//!
//! Both seams ([`Forge`], [`AgentRunner`]) are traits, so the whole loop is
//! exercised hermetically against local repos with a fake runner.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::engine::{AgentInvocation, AgentRunner};
use crate::forge::{Forge, PrRequest};
use crate::ir::{Ir, Reaction};

/// A GitHub `issues` webhook event, normalized to what a reaction needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueEvent {
    /// `owner/name`.
    pub repo: String,
    /// The `action` (e.g. `opened` / `labeled`).
    pub action: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    /// The issue's HTML URL.
    pub url: String,
    /// All labels on the issue (for a `labeled` event the just-added label is
    /// included too, so a label filter matches regardless of action).
    pub labels: Vec<String>,
    /// The repo's default branch (PR base + the branch the prototype forks from).
    pub default_branch: String,
    /// The App installation id, needed to mint a token for owned repos.
    pub installation_id: Option<u64>,
}

/// Issue actions a reaction reacts to when its rule sets no explicit `action`.
const DEFAULT_ACTIONS: &[&str] = &["opened", "labeled"];

/// Parse a GitHub `issues` webhook payload into an [`IssueEvent`].
pub fn parse_webhook_issue(v: &serde_json::Value) -> Option<IssueEvent> {
    let repo = v.get("repository")?.get("full_name")?.as_str()?.to_owned();
    let action = v.get("action")?.as_str()?.to_owned();
    let issue = v.get("issue")?;
    let number = issue.get("number")?.as_u64()?;
    let title = issue
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_owned();
    let body = issue
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or_default()
        .to_owned();
    let url = issue
        .get("html_url")
        .and_then(|u| u.as_str())
        .unwrap_or_default()
        .to_owned();

    let mut labels: Vec<String> = issue
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // A `labeled` event carries the added label separately; fold it in so a
    // label filter matches even if the issue's `labels` array lags.
    if let Some(name) = v.get("label").and_then(|l| l.get("name")).and_then(|n| n.as_str()) {
        if !labels.iter().any(|x| x == name) {
            labels.push(name.to_owned());
        }
    }

    let default_branch = v
        .get("repository")
        .and_then(|r| r.get("default_branch"))
        .and_then(|b| b.as_str())
        .unwrap_or("main")
        .to_owned();
    let installation_id = v
        .get("installation")
        .and_then(|i| i.get("id"))
        .and_then(|i| i.as_u64());

    Some(IssueEvent {
        repo,
        action,
        number,
        title,
        body,
        url,
        labels,
        default_branch,
        installation_id,
    })
}

/// `owner/name` → reactions, built from a monorepo's IR. The reaction-side
/// analogue of [`crate::server::Index`].
#[derive(Debug, Default)]
pub struct ReactionIndex<'a>(pub BTreeMap<String, Vec<&'a Reaction>>);

impl<'a> ReactionIndex<'a> {
    pub fn from_ir(ir: &'a Ir) -> Self {
        let mut map: BTreeMap<String, Vec<&Reaction>> = BTreeMap::new();
        for r in &ir.reactions {
            map.entry(r.repo.clone()).or_default().push(r);
        }
        ReactionIndex(map)
    }
}

/// Whether a reaction's filters match an issue event.
fn reaction_matches(r: &Reaction, ev: &IssueEvent) -> bool {
    let action_ok = match &r.action {
        Some(a) => a == &ev.action,
        None => DEFAULT_ACTIONS.contains(&ev.action.as_str()),
    };
    let label_ok = match &r.label_filter {
        Some(l) => ev.labels.iter().any(|x| x == l),
        None => true,
    };
    action_ok && label_ok
}

/// Reactions in `index` that match `ev` (filtered by action + label).
pub fn match_issue<'a>(index: &ReactionIndex<'a>, ev: &IssueEvent) -> Vec<&'a Reaction> {
    index
        .0
        .get(&ev.repo)
        .into_iter()
        .flatten()
        .copied()
        .filter(|r| reaction_matches(r, ev))
        .collect()
}

/// The outcome of running one reaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionOutcome {
    /// The branch pushed (when the agent made changes).
    pub branch: Option<String>,
    /// A one-line, human-readable summary (also the webhook response line).
    pub summary: String,
}

/// Resolve a reaction's `agent`/`prompt_template` (and `vars`) into a runnable
/// [`AgentInvocation`] plus the fully-rendered prompt (with issue context vars
/// substituted). Mirrors `reconcile::resolve_agent_invocation`, but the context
/// is the issue rather than an import diff. Public so the `react --dry-run` CLI
/// can preview the resolved agent + prompt without cloning or running anything.
pub fn resolve_reaction_invocation(
    ir: &Ir,
    reaction: &Reaction,
    ev: &IssueEvent,
    root: &Path,
) -> Result<(AgentInvocation, String)> {
    let agent = ir
        .agents
        .iter()
        .find(|a| a.label == reaction.agent)
        .ok_or_else(|| anyhow::anyhow!("agent `{}` does not resolve", reaction.agent))?;
    let harness = ir
        .harnesses
        .iter()
        .find(|h| h.label == agent.harness)
        .ok_or_else(|| anyhow::anyhow!("harness `{}` does not resolve", agent.harness))?;
    let model = ir
        .models
        .iter()
        .find(|m| m.label == agent.model)
        .ok_or_else(|| anyhow::anyhow!("model `{}` does not resolve", agent.model))?;
    let prompt_template = ir
        .prompt_templates
        .iter()
        .find(|p| p.label == reaction.prompt_template)
        .ok_or_else(|| {
            anyhow::anyhow!("prompt_template `{}` does not resolve", reaction.prompt_template)
        })?;

    let kind = crate::agent::HarnessKind::parse(&harness.kind)?;

    // Read the template, substitute user vars (a `//`-anchored value is a file
    // label read from disk; a plain string is literal), then the issue vars.
    let mut prompt = std::fs::read_to_string(root.join(&prompt_template.src))
        .with_context(|| format!("reading prompt template {}", prompt_template.src))?;
    for (key, value) in &reaction.vars {
        let rendered = if let Some(rest) = value.strip_prefix("//") {
            let rel = match rest.split_once(':') {
                Some(("", name)) => name.to_owned(),
                Some((pkg, name)) => format!("{pkg}/{name}"),
                None => rest.to_owned(),
            };
            std::fs::read_to_string(root.join(&rel))
                .with_context(|| format!("reading var `{key}` file {value}"))?
        } else {
            value.clone()
        };
        prompt = prompt.replace(&format!("{{{{{key}}}}}"), &rendered);
    }
    prompt = render_issue_vars(&prompt, ev, &reaction.repo);

    let inv = AgentInvocation {
        harness: kind,
        provider: model.provider.clone(),
        model_id: model.id.clone(),
        credential: model.credential.clone(),
        base_url: None,
        prompt: prompt.clone(),
        agent_id: agent.label.clone(),
        paths: Vec::new(),
    };
    Ok((inv, prompt))
}

/// Substitute the issue context `{{var}}` tokens. Unknown tokens are left intact.
fn render_issue_vars(prompt: &str, ev: &IssueEvent, repo: &str) -> String {
    prompt
        .replace("{{issue_title}}", &ev.title)
        .replace("{{issue_body}}", &ev.body)
        .replace("{{issue_number}}", &ev.number.to_string())
        .replace("{{issue_url}}", &ev.url)
        .replace("{{repo}}", repo)
}

/// Run one reaction end-to-end: clone → branch → agent → commit → push → PR.
/// Idempotent at the no-change boundary (an agent that edits nothing opens no
/// PR). Returns a one-line summary.
pub fn run_reaction(
    forge: &dyn Forge,
    runner: &dyn AgentRunner,
    ir: &Ir,
    reaction: &Reaction,
    ev: &IssueEvent,
    root: &Path,
) -> Result<ReactionOutcome> {
    let (inv, prompt) = resolve_reaction_invocation(ir, reaction, ev, root)?;

    let url = forge.git_url(&reaction.repo, ev.installation_id)?;
    let workdir = tempfile::tempdir().context("creating reaction workdir")?;
    let checkout = workdir.path().join("checkout");

    // Clone the repo at its default branch.
    git(workdir.path(), &["clone", "--quiet", &url, "checkout"])
        .with_context(|| format!("cloning {} for reaction {}", reaction.repo, reaction.label))?;

    let branch = format!("capyfun/issue-{}", ev.number);
    git(&checkout, &["checkout", "--quiet", "-b", &branch])?;

    // Run the agent over the checkout; it edits files in place.
    runner
        .run(&inv, &prompt, &checkout)
        .with_context(|| format!("running agent {} for issue #{}", inv.agent_id, ev.number))?;

    // No edits → no PR (idempotent at the no-change boundary).
    let status = git(&checkout, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(ReactionOutcome {
            branch: None,
            summary: format!(
                "reaction {}: agent made no changes for issue #{}; no PR",
                reaction.label, ev.number
            ),
        });
    }

    // Commit the agent's edits, attributed to CapyFun with provenance trailers.
    git(&checkout, &["add", "-A"])?;
    let message = format!(
        "Prototype issue #{n}: {title}\n\n\
         Drafted by CapyFun in response to {url}\n\n\
         Closes #{n}\n\n\
         CapyFun-Agent: {agent}\n\
         CapyFun-Issue: {repo}#{n}\n",
        n = ev.number,
        title = ev.title,
        url = ev.url,
        agent = inv.agent_id,
        repo = reaction.repo,
    );
    git(
        &checkout,
        &[
            "-c",
            "user.name=CapyFun",
            "-c",
            "user.email=capyfun@users.noreply.github.com",
            "commit",
            "--quiet",
            "-m",
            &message,
        ],
    )?;

    // Push the branch back to the origin we cloned from.
    git(&checkout, &["push", "--quiet", "origin", &branch])
        .with_context(|| format!("pushing branch {branch}"))?;

    let pr = PrRequest {
        base: ev.default_branch.clone(),
        head: branch.clone(),
        title: format!("Prototype issue #{}: {}", ev.number, ev.title),
        body: format!(
            "Automated prototype by CapyFun for {} (agent `{}`).\n\nCloses #{}",
            ev.url, inv.agent_id, ev.number
        ),
    };
    let outcome = forge
        .open_pr(&reaction.repo, ev.installation_id, &pr)
        .with_context(|| format!("opening PR for issue #{}", ev.number))?;

    Ok(ReactionOutcome {
        branch: Some(branch),
        summary: format!("reaction {}: {}", reaction.label, outcome.summary),
    })
}

/// Run `git` with `args` in `dir`, returning stdout on success.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("spawning `git {}`", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests;
