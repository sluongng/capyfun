//! Reconcile: run the idempotent import/vendor/export action needed to bring a
//! target to its desired upstream state.
//!
//! This is the *acting* half of the level-triggered model (the read-only
//! [`crate::status`] is the dry-run half). It is shared by the `reconcile` CLI
//! command and the automation server (which acts on a matched trigger), so the
//! two cannot drift. Each action is idempotent — a no-op when there is nothing
//! new — built on the engine's commit map.

use std::path::Path;

use anyhow::{Context, Result};
use git2::Repository;

use crate::ir::{Export, Import, Ir, Vendor};

/// Knobs for a reconcile action.
#[derive(Default)]
pub struct Options {
    /// Import: bypass the agent-output cache and re-run every `agent_transform`.
    pub refresh: bool,
    /// Export: push the branch but do not open a GitHub PR (print the command).
    pub no_pr: bool,
    /// Where `agent_transform`s run (local harness vs. REAPI/BuildBuddy).
    pub executor: Executor,
}

/// Default container image for remote execution (must contain the agent harness
/// and `sh`; override with `BUILDBUDDY_EXEC_IMAGE`).
const DEFAULT_EXEC_IMAGE: &str = "docker://mirror.gcr.io/library/busybox:latest";

/// Where the generative (`agent_transform`) steps of an import run.
///
/// Structural transforms, the mirror replay, vendors, and exports are always
/// local Git work; only the agent harness can be offloaded. With [`Remote`], each
/// `agent_transform` becomes a REAPI Action on the BuildBuddy pool and repeat
/// runs are served from the Action Cache (see `docs/design/remote-execution.md`).
///
/// [`Remote`]: Executor::Remote
#[derive(Default)]
pub enum Executor {
    /// Run the harness locally (the default; [`crate::engine::LiveRunner`]).
    #[default]
    Local,
    /// Run the harness on a REAPI backend ([`crate::engine::RemoteRunner`]).
    Remote(RemoteSettings),
}

/// Connection + placement settings for the remote executor.
pub struct RemoteSettings {
    pub config: crate::remote::client::RemoteConfig,
    /// Platform properties selecting the executor (container image, OS, network).
    pub platform: Vec<(String, String)>,
}

impl RemoteSettings {
    /// Build from the environment: the `BUILDBUDDY_*` connection vars (see
    /// [`RemoteConfig::from_env`](crate::remote::client::RemoteConfig::from_env))
    /// plus `BUILDBUDDY_EXEC_IMAGE` for the executor container.
    pub fn from_env() -> Self {
        let image =
            std::env::var("BUILDBUDDY_EXEC_IMAGE").unwrap_or_else(|_| DEFAULT_EXEC_IMAGE.to_owned());
        RemoteSettings {
            config: crate::remote::client::RemoteConfig::from_env(),
            platform: platform_props(&image),
        }
    }
}

/// The platform properties for a Linux executor running `image`. Pure, so the
/// placement is unit-testable without the environment.
fn platform_props(image: &str) -> Vec<(String, String)> {
    vec![
        ("OSFamily".to_owned(), "Linux".to_owned()),
        ("container-image".to_owned(), image.to_owned()),
    ]
}

/// Resolve a GitHub `owner/name` slug to a fetchable URL (honoring
/// `CAPYFUN_GITHUB_BASE` for hermetic demos/tests).
pub fn origin_url(slug: &str) -> String {
    crate::vendorgen::github_url(slug)
}

/// Reconcile a single target by label, searching imports, then vendors, then
/// exports. Returns the one-line summary. Errors if the label resolves to no
/// target.
pub fn reconcile_label(
    repo: &Repository,
    ir: &Ir,
    root: &Path,
    label: &str,
    opts: Options,
) -> Result<String> {
    if let Some(i) = ir.imports.iter().find(|i| i.label == label) {
        return do_import(repo, ir, i, root, opts.refresh, &opts.executor);
    }
    if let Some(v) = ir.vendors.iter().find(|v| v.label == label) {
        return do_vendor(repo, ir, v);
    }
    if let Some(e) = ir.exports.iter().find(|e| e.label == label) {
        return do_export(repo, ir, e, opts.no_pr);
    }
    anyhow::bail!("no target labeled `{label}`")
}

/// Run one import: fetch the origin, (re)apply the mirror + tip layers via the
/// engine, and advance the monorepo branch. Idempotent — a no-op when there is
/// nothing new. Returns a one-line summary.
pub fn do_import(
    repo: &Repository,
    ir: &Ir,
    import: &Import,
    root: &Path,
    refresh: bool,
    executor: &Executor,
) -> Result<String> {
    // Current tip of the monorepo branch we import onto.
    let branch_ref = format!("refs/heads/{}", ir.monorepo.default_branch);
    let branch_tip = repo.find_reference(&branch_ref).ok().and_then(|r| r.target());

    // Fetch the origin ref into the monorepo's object store.
    let url = origin_url(&import.repo);
    let origin_tip = crate::engine::fetch_commit(repo, &url, &import.git_ref)?;

    // Read the patch series from the working tree.
    let patches = import
        .patches
        .iter()
        .map(|p| {
            let bytes =
                std::fs::read(root.join(p)).with_context(|| format!("reading patch {p}"))?;
            Ok(crate::engine::PatchFile {
                label: p.clone(),
                bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // Resolve the declared tip-phase transforms (ordered: apply_patch +
    // agent_transform) into engine-facing structs. IR resolution and file reads
    // live here so the engine stays decoupled from `ir`.
    let tips = resolve_tip_transforms(ir, import, root)?;

    // Select where agent_transforms run. The remote runner borrows a connected
    // client, so connect inside the match arm and run the import there.
    let outcome = match executor {
        Executor::Local => {
            let runner = crate::engine::LiveRunner;
            run_import_engine(repo, import, origin_tip, branch_tip, &patches, &tips, refresh, &runner)?
        }
        Executor::Remote(settings) => {
            let client = crate::remote::client::RemoteClient::connect(&settings.config)
                .context("connecting to the remote executor")?;
            let runner =
                crate::engine::RemoteRunner::new(&client).with_platform(settings.platform.clone());
            run_import_engine(repo, import, origin_tip, branch_tip, &patches, &tips, refresh, &runner)?
        }
    };

    match outcome.head {
        Some(head) if Some(head) != branch_tip => {
            repo.reference(
                &branch_ref,
                head,
                true,
                &format!("capyfun import {}", import.label),
            )?;
            let t = &outcome.tip;
            Ok(format!(
                "imported {} commit(s) into {}; {branch_ref} now {head} \
                 (tip: {} patch, {} agent, cache {}h/{}m)",
                outcome.imported,
                import.dest,
                t.patch_commits,
                t.agent_commits,
                t.agent_cache_hits,
                t.agent_cache_misses
            ))
        }
        _ => Ok("already up to date".to_owned()),
    }
}

/// Drive the engine import with a chosen [`AgentRunner`](crate::engine::AgentRunner)
/// (local or remote). Splitting this out lets `do_import` select the runner —
/// whose lifetime is tied to a connected client — without duplicating the import
/// body.
#[allow(clippy::too_many_arguments)]
fn run_import_engine(
    repo: &Repository,
    import: &Import,
    origin_tip: git2::Oid,
    branch_tip: Option<git2::Oid>,
    patches: &[crate::engine::PatchFile],
    tips: &[crate::engine::TipTransform],
    refresh: bool,
    runner: &dyn crate::engine::AgentRunner,
) -> Result<crate::engine::FullImportOutcome> {
    let tip_layer = crate::engine::TipLayer {
        patches,
        tips,
        runner,
        refresh,
    };
    crate::engine::import(
        repo,
        &import.dest,
        origin_tip,
        branch_tip,
        &import.transforms,
        &tip_layer,
    )
}

/// Vendor one pinned snapshot: fetch the declared commit and (re)materialize its
/// tree into the package, advancing the monorepo branch. Idempotent. Returns a
/// one-line summary.
pub fn do_vendor(repo: &Repository, ir: &Ir, vendor: &Vendor) -> Result<String> {
    let branch_ref = format!("refs/heads/{}", ir.monorepo.default_branch);
    let branch_tip = repo.find_reference(&branch_ref).ok().and_then(|r| r.target());

    let url = origin_url(&vendor.repo);
    let commit = crate::engine::fetch_commit(repo, &url, &vendor.commit)?;
    let outcome =
        crate::engine::vendor_snapshot(repo, &vendor.dest, &vendor.repo, commit, branch_tip)?;

    match outcome.head {
        Some(head) if Some(head) != branch_tip => {
            repo.reference(
                &branch_ref,
                head,
                true,
                &format!("capyfun vendor {}", vendor.label),
            )?;
            Ok(format!(
                "vendored {}@{} into {}; {branch_ref} now {head}",
                vendor.repo, vendor.commit, vendor.dest
            ))
        }
        _ => Ok(format!("already vendored at {}", vendor.commit)),
    }
}

/// Run one export: project the monorepo path onto the destination (prefix
/// stripped, transforms applied), push the export branch, and open a PR (unless
/// suppressed). Idempotent — a no-op when nothing new has landed. Returns a
/// one-line summary (preserving the `already up to date` phrasing).
pub fn do_export(repo: &Repository, ir: &Ir, export: &Export, no_pr: bool) -> Result<String> {
    // Current tip of the monorepo branch we export from.
    let mono_ref = format!("refs/heads/{}", ir.monorepo.default_branch);
    let mono_tip = repo
        .find_reference(&mono_ref)
        .ok()
        .and_then(|r| r.target())
        .with_context(|| format!("monorepo branch {mono_ref} has no commits to export"))?;

    // Fetch the destination branch into the object store: it is the commit-map
    // source of truth for what has already shipped. A destination with no such
    // branch yet (a fresh repo) means nothing has shipped.
    let url = origin_url(&export.repo);
    let dest_tip =
        crate::engine::fetch_commit(repo, &url, &format!("refs/heads/{}", export.branch)).ok();

    let outcome =
        crate::engine::export(repo, &export.from, mono_tip, dest_tip, &export.transforms)?;

    match outcome.head {
        Some(head) if Some(head) != dest_tip => {
            let export_branch = format!("capyfun/export-{}", export.name);
            crate::engine::push_branch(repo, &url, head, &export_branch)?;
            open_pr(export, &export_branch, no_pr)?;
            Ok(format!(
                "exported {} commit(s) from {}; pushed branch {} to {}",
                outcome.exported, export.from, export_branch, export.repo
            ))
        }
        _ => Ok(format!("already up to date on {}", export.repo)),
    }
}

/// Resolve an import's declared tip-phase transforms into engine `TipTransform`s.
///
/// `apply_patch` reads the patch bytes from the working tree (mirroring the
/// `patches=[]` handling); `agent_transform` resolves the agent label →
/// harness/model, reads the prompt-template `.tmpl` file, and substitutes the
/// user `vars` (file-label vars are read from disk). Engine-derived context vars
/// are filled later by the engine.
fn resolve_tip_transforms(
    ir: &Ir,
    import: &Import,
    root: &Path,
) -> Result<Vec<crate::engine::TipTransform>> {
    use crate::transform::Transform;

    let mut out = Vec::new();
    for t in &import.transforms {
        match t {
            Transform::ApplyPatch { file } => {
                let bytes = std::fs::read(root.join(file))
                    .with_context(|| format!("reading apply_patch file {file}"))?;
                out.push(crate::engine::TipTransform::Patch(crate::engine::PatchFile {
                    label: file.clone(),
                    bytes,
                }));
            }
            Transform::AgentTransform {
                agent,
                prompt_template,
                vars,
                paths,
            } => {
                let inv = resolve_agent_invocation(ir, agent, prompt_template, vars, paths, root)?;
                out.push(crate::engine::TipTransform::Agent(inv));
            }
            // Mirror-phase transforms are applied per commit, not in the tip.
            _ => {}
        }
    }
    Ok(out)
}

/// Resolve one `agent_transform` into an `AgentInvocation`: agent label →
/// harness kind + model (provider/id/credential), read the prompt-template file,
/// and substitute the user `vars` (a `//`-label var value is read from the file
/// it points at; a plain string is used verbatim).
fn resolve_agent_invocation(
    ir: &Ir,
    agent_label: &str,
    prompt_template_label: &str,
    vars: &[(String, String)],
    paths: &[String],
    root: &Path,
) -> Result<crate::engine::AgentInvocation> {
    let agent = ir
        .agents
        .iter()
        .find(|a| a.label == agent_label)
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_label}` does not resolve"))?;
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
        .find(|p| p.label == prompt_template_label)
        .ok_or_else(|| {
            anyhow::anyhow!("prompt_template `{prompt_template_label}` does not resolve")
        })?;

    let kind = crate::agent::HarnessKind::parse(&harness.kind)?;

    // Read the template, then substitute the user vars (engine fills context vars).
    let mut prompt = std::fs::read_to_string(root.join(&prompt_template.src))
        .with_context(|| format!("reading prompt template {}", prompt_template.src))?;
    for (key, value) in vars {
        // A `//`-anchored value is a file label: read its contents; otherwise the
        // value is a literal string.
        let rendered = if let Some(rest) = value.strip_prefix("//") {
            // `//docs:STYLE.md` -> `docs/STYLE.md`; `//path/to/file` -> as-is.
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

    Ok(crate::engine::AgentInvocation {
        harness: kind,
        provider: model.provider.clone(),
        model_id: model.id.clone(),
        credential: model.credential.clone(),
        base_url: None,
        prompt,
        agent_id: agent.label.clone(),
        paths: paths.to_vec(),
    })
}

/// Open a GitHub PR for a pushed export branch, or explain why it was skipped.
///
/// PR creation shells out to the GitHub CLI (`gh`). It is skipped — printing the
/// equivalent command instead — when `no_pr` is set or the destination is a local
/// repository (`CAPYFUN_GITHUB_BASE`), so hermetic demos/tests exercise the full
/// branch push without needing network access or a real forge.
fn open_pr(export: &Export, branch: &str, no_pr: bool) -> Result<()> {
    let title = format!("Export {} from the monorepo", export.name);
    let body = format!(
        "Automated export by CapyFun from `{}`.\n\n\
         Each commit carries a `CapyFun-Export` trailer mapping it back to the \
         monorepo commit it reflects.",
        export.from
    );

    let local_dest = std::env::var("CAPYFUN_GITHUB_BASE").is_ok();
    if no_pr || local_dest {
        let why = if no_pr {
            "--no-pr"
        } else {
            "local destination (CAPYFUN_GITHUB_BASE)"
        };
        println!("skipping PR ({why}); to open it yourself, run:");
        println!(
            "  gh pr create --repo {} --base {} --head {} --title {:?}",
            export.repo, export.branch, branch, title
        );
        return Ok(());
    }

    let status = std::process::Command::new("gh")
        .args([
            "pr", "create", "--repo", &export.repo, "--base", &export.branch, "--head", branch,
            "--title", &title, "--body", &body,
        ])
        .status()
        .context("running `gh pr create` (is the GitHub CLI installed and authenticated?)")?;
    if !status.success() {
        anyhow::bail!("`gh pr create` failed");
    }
    println!("opened PR: {} <- {} on {}", export.branch, branch, export.repo);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executor_defaults_to_local() {
        assert!(matches!(Executor::default(), Executor::Local));
        assert!(matches!(Options::default().executor, Executor::Local));
    }

    #[test]
    fn platform_props_select_linux_and_image() {
        let p = platform_props("docker://example/img:tag");
        assert_eq!(
            p,
            vec![
                ("OSFamily".to_owned(), "Linux".to_owned()),
                ("container-image".to_owned(), "docker://example/img:tag".to_owned()),
            ]
        );
    }

    #[test]
    fn remote_settings_from_env_uses_default_image_and_props() {
        // BUILDBUDDY_EXEC_IMAGE is not set in the hermetic test environment, so
        // the default image is used and the standard Linux props are produced.
        let s = RemoteSettings::from_env();
        assert!(s
            .platform
            .iter()
            .any(|(k, v)| k == "container-image" && v == DEFAULT_EXEC_IMAGE));
        assert!(s
            .platform
            .iter()
            .any(|(k, v)| k == "OSFamily" && v == "Linux"));
    }
}
