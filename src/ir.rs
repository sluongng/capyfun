//! Normalized CapyFun IR.
//!
//! Lowers captured config [`Decl`](crate::config::Decl)s into a deterministic,
//! validated intermediate representation: labels resolved, destination/source
//! paths package-anchored (monorepo-root-relative), and statically checked
//! before any Git operation runs.

use serde::Serialize;

use crate::config::{Decl, RawConfig, TransformSpec};
use crate::transform::{Phase, Transform};
use crate::validate;

/// The validated, normalized configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ir {
    pub monorepo: Monorepo,
    pub imports: Vec<Import>,
    pub vendors: Vec<Vendor>,
    pub exports: Vec<Export>,
    pub harnesses: Vec<Harness>,
    pub models: Vec<Model>,
    pub agents: Vec<Agent>,
    pub prompt_templates: Vec<PromptTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Monorepo {
    pub name: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Import {
    /// Bazel-style label, e.g. `//third_party/backend:backend`.
    pub label: String,
    pub name: String,
    pub package: String,
    /// GitHub `owner/name` slug.
    pub repo: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Monorepo-root-relative destination directory the import lands in.
    pub dest: String,
    /// Monorepo-root-relative patch files (resolved against the SRC package).
    pub patches: Vec<String>,
    /// Validated transform pipeline, ordered mirror-phase first then tip-phase,
    /// preserving source order within each phase. All paths/text are
    /// subtree-relative (not package-anchored); they apply to the imported
    /// subtree before it is spliced under `dest`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<Transform>,
}

/// A pinned-snapshot git dependency (`git_repository`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Vendor {
    pub label: String,
    pub name: String,
    pub package: String,
    /// GitHub `owner/name` slug.
    pub repo: String,
    /// Exact commit SHA (the pin).
    pub commit: String,
    /// Monorepo-root-relative destination directory the snapshot lands in.
    pub dest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Export {
    pub label: String,
    pub name: String,
    pub package: String,
    pub repo: String,
    pub branch: String,
    /// Monorepo-root-relative source directory to export.
    pub from: String,
    /// Validated structural transform pipeline applied to the exported subtree
    /// before it ships (e.g. scrubbing internal-only lines). Subtree-relative,
    /// in source order. Only mirror-phase transforms are valid on export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<Transform>,
}

/// An agent harness runtime (`harness`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Harness {
    /// Bazel-style label, e.g. `//tools/harness:claude_code`.
    pub label: String,
    pub name: String,
    pub package: String,
    /// Harness runtime kind (validated against [`crate::agent::HarnessKind`]).
    pub kind: String,
    /// Labels of `git_repository` plugin targets brought in as runfiles.
    pub plugins: Vec<String>,
    /// Labels of `git_repository` skill targets brought in as runfiles.
    pub skills: Vec<String>,
}

/// An LLM (`model`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Model {
    /// Bazel-style label, e.g. `//tools/models:opus`.
    pub label: String,
    pub name: String,
    pub package: String,
    /// Provider, e.g. `anthropic` / `openai` / `google` / `nebius`.
    pub provider: String,
    /// Provider-specific model id.
    pub id: String,
    /// Optional credential reference (e.g. `env:NAME`); never a secret value.
    pub credential: Option<String>,
}

/// An agent (`agent`): a resolved `harness` + `model` pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Agent {
    /// Bazel-style label, e.g. `//tools/agent:reviewer`.
    pub label: String,
    pub name: String,
    pub package: String,
    /// Resolved label of the `harness` rule this agent runs on.
    pub harness: String,
    /// Resolved label of the `model` rule this agent drives.
    pub model: String,
}

/// A prompt template (`prompt_template`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptTemplate {
    /// Bazel-style label, e.g. `//tools/agent/prompts:review`.
    pub label: String,
    pub name: String,
    pub package: String,
    /// Monorepo-root-relative `.tmpl` path (resolved against the SRC package).
    pub src: String,
}

/// The monorepo-root-relative directory for a package label (`//` -> "").
fn package_dir(package: &str) -> &str {
    package.strip_prefix("//").unwrap_or(package)
}

/// Join a package directory with an optional subpath, keeping forward slashes.
fn join_dir(dir: &str, sub: Option<&str>) -> String {
    match sub {
        Some(sub) if dir.is_empty() => sub.to_owned(),
        Some(sub) => format!("{dir}/{sub}"),
        None => dir.to_owned(),
    }
}

/// Lower and validate a [`RawConfig`] into [`Ir`].
///
/// Returns all diagnostics (sorted) on failure rather than stopping at the
/// first, so a single run surfaces every problem.
pub fn compile(raw: &RawConfig) -> Result<Ir, Vec<String>> {
    let mut errors = Vec::new();

    // --- monorepo singleton, in the root package ---
    let monorepos: Vec<_> = raw
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Monorepo(m) => Some(m),
            _ => None,
        })
        .collect();
    let monorepo = match monorepos.as_slice() {
        [] => {
            errors.push("no monorepo(...) declared; the root SRC must declare exactly one".into());
            None
        }
        [m] => {
            if m.package != "//" {
                errors.push(format!(
                    "monorepo(...) must be declared in the root SRC (//), not in {}",
                    m.package
                ));
            }
            Some(Monorepo {
                name: m.name.clone(),
                default_branch: m.default_branch.clone(),
            })
        }
        many => {
            let pkgs: Vec<&str> = many.iter().map(|m| m.package.as_str()).collect();
            errors.push(format!(
                "monorepo(...) declared {} times (in {}); exactly one is allowed",
                many.len(),
                pkgs.join(", ")
            ));
            None
        }
    };

    // --- lower imports/vendors/exports + agent tool rules ---
    let mut imports = Vec::new();
    let mut vendors = Vec::new();
    let mut exports = Vec::new();
    let mut harnesses = Vec::new();
    let mut models = Vec::new();
    let mut prompt_templates = Vec::new();
    // Agents are lowered in a second pass: resolving `harness`/`model` labels
    // needs the harnesses/models above to be lowered first.
    let mut agent_decls = Vec::new();
    for decl in &raw.decls {
        match decl {
            Decl::Monorepo(_) => {}
            Decl::GitRepo(d) => {
                let label = format!("{}:{}", d.package, d.name);
                validate::check_slug(&label, &d.repo, &mut errors);
                validate::check_commit_sha(&label, &d.commit, &mut errors);
                if let Some(into) = &d.into {
                    validate::check_rel_path(&label, "into", into, &mut errors);
                }
                let dir = package_dir(&d.package);
                let dest = join_dir(dir, d.into.as_deref());
                if dest.is_empty() {
                    errors.push(format!(
                        "{label}: git_repository targets the monorepo root; declare it in a sub-package or set `into`"
                    ));
                }
                vendors.push(Vendor {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    repo: d.repo.clone(),
                    commit: d.commit.clone(),
                    dest,
                });
            }
            Decl::Import(d) => {
                let label = format!("{}:{}", d.package, d.name);
                validate::check_slug(&label, &d.repo, &mut errors);
                if d.git_ref.is_empty() {
                    errors.push(format!("{label}: ref is empty"));
                }
                if let Some(into) = &d.into {
                    validate::check_rel_path(&label, "into", into, &mut errors);
                }
                let dir = package_dir(&d.package);
                let dest = join_dir(dir, d.into.as_deref());
                if dest.is_empty() {
                    errors.push(format!(
                        "{label}: import targets the monorepo root; declare it in a sub-package or set `into`"
                    ));
                }
                let patches = d
                    .patches
                    .iter()
                    .map(|p| {
                        validate::check_rel_path(&label, "patch", p, &mut errors);
                        join_dir(dir, Some(p))
                    })
                    .collect();
                let transforms = lower_transforms(&label, dir, &d.transforms, &mut errors);
                imports.push(Import {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    repo: d.repo.clone(),
                    git_ref: d.git_ref.clone(),
                    dest,
                    patches,
                    transforms,
                });
            }
            Decl::Export(d) => {
                let label = format!("{}:{}", d.package, d.name);
                validate::check_slug(&label, &d.repo, &mut errors);
                if d.branch.is_empty() {
                    errors.push(format!("{label}: branch is empty"));
                }
                if let Some(from) = &d.from_path {
                    validate::check_rel_path(&label, "from_path", from, &mut errors);
                }
                let dir = package_dir(&d.package);
                let from = join_dir(dir, d.from_path.as_deref());
                if from.is_empty() {
                    errors.push(format!(
                        "{label}: export sources the monorepo root; declare it in a sub-package or set `from_path`"
                    ));
                }
                let transforms = lower_transforms(&label, dir, &d.transforms, &mut errors);
                // Tip-phase transforms (`apply_patch`, `agent_transform`) are an
                // import-only local-modification layer; they have no meaning when
                // projecting a path out. Reject them so a misplaced one is loud.
                for t in &transforms {
                    if t.phase() == Phase::Tip {
                        errors.push(format!(
                            "{label}: transform `{}` is not valid on export (only structural \
                             transforms — replace/move/copy/rewrite_message — apply when \
                             exporting)",
                            t.kind()
                        ));
                    }
                }
                exports.push(Export {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    repo: d.repo.clone(),
                    branch: d.branch.clone(),
                    from,
                    transforms,
                });
            }
            Decl::Harness(d) => {
                let label = format!("{}:{}", d.package, d.name);
                if let Err(e) = crate::agent::HarnessKind::parse(&d.kind) {
                    errors.push(format!("{label}: {e}"));
                }
                harnesses.push(Harness {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    kind: d.kind.clone(),
                    plugins: d.plugins.clone(),
                    skills: d.skills.clone(),
                });
            }
            Decl::Model(d) => {
                let label = format!("{}:{}", d.package, d.name);
                if !crate::agent::is_known_provider(&d.provider) {
                    errors.push(format!(
                        "{label}: provider `{}` is unknown (known: {})",
                        d.provider,
                        crate::agent::KNOWN_PROVIDERS.join(", ")
                    ));
                }
                if d.id.is_empty() {
                    errors.push(format!("{label}: model id is empty"));
                }
                // Validate the credential *reference shape* only (e.g. `env:NAME`);
                // whether the variable is set is an execution-time concern.
                if let Some(reference) = &d.credential {
                    if let Err(e) =
                        crate::agent::credential_var(&d.provider, Some(reference.as_str()))
                    {
                        errors.push(format!("{label}: {e}"));
                    }
                }
                models.push(Model {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    provider: d.provider.clone(),
                    id: d.id.clone(),
                    credential: d.credential.clone(),
                });
            }
            Decl::PromptTemplate(d) => {
                let label = format!("{}:{}", d.package, d.name);
                // Config evaluation is pure (no I/O): validate the path *shape*
                // only (relative, no `..`). Actual file existence is an
                // execution-time check, deferred to the engine.
                validate::check_rel_path(&label, "src", &d.src, &mut errors);
                let dir = package_dir(&d.package);
                let src = join_dir(dir, Some(&d.src));
                prompt_templates.push(PromptTemplate {
                    label,
                    name: d.name.clone(),
                    package: d.package.clone(),
                    src,
                });
            }
            Decl::Agent(d) => agent_decls.push(d),
        }
    }

    // --- second pass: resolve agent harness/model labels ---
    let mut agents = Vec::new();
    for d in agent_decls {
        let label = format!("{}:{}", d.package, d.name);
        let harness = resolve_label(
            &label,
            "harness",
            &d.harness,
            harnesses.iter().map(|h| h.label.as_str()),
            &mut errors,
        );
        let model = resolve_label(
            &label,
            "model",
            &d.model,
            models.iter().map(|m| m.label.as_str()),
            &mut errors,
        );
        // If both resolved, check the harness can drive the model's provider.
        if let (Some(h_label), Some(m_label)) = (&harness, &model) {
            let h = harnesses
                .iter()
                .find(|h| &h.label == h_label)
                .expect("resolved harness label exists");
            let m = models
                .iter()
                .find(|m| &m.label == m_label)
                .expect("resolved model label exists");
            if let Ok(kind) = crate::agent::HarnessKind::parse(&h.kind) {
                if !kind.can_drive(&m.provider) {
                    errors.push(format!(
                        "{label}: harness `{}` (kind {}) cannot drive model `{}` (provider {})",
                        h.label, h.kind, m.label, m.provider
                    ));
                }
            }
        }
        agents.push(Agent {
            label,
            name: d.name.clone(),
            package: d.package.clone(),
            harness: harness.unwrap_or_else(|| d.harness.clone()),
            model: model.unwrap_or_else(|| d.model.clone()),
        });
    }

    // --- third pass: resolve agent_transform label references ---
    // `agent_transform`s carry `agent` / `prompt_template` labels that can only
    // be checked once agents and prompt templates exist.
    for import in &imports {
        for t in &import.transforms {
            if let Transform::AgentTransform {
                agent,
                prompt_template,
                vars,
                ..
            } = t
            {
                resolve_label(
                    &import.label,
                    "agent",
                    agent,
                    agents.iter().map(|a| a.label.as_str()),
                    &mut errors,
                );
                resolve_label(
                    &import.label,
                    "prompt_template",
                    prompt_template,
                    prompt_templates.iter().map(|p| p.label.as_str()),
                    &mut errors,
                );
                // Var *values* that look like `//`-anchored labels are shape-
                // checked only (purity: no filesystem/target lookup here).
                for (k, v) in vars {
                    if v.starts_with("//") && !is_label_shaped(v) {
                        errors.push(format!(
                            "{}: agent_transform var `{k}` value `{v}` is not a valid label",
                            import.label
                        ));
                    }
                }
            }
        }
    }

    // --- cross-rule checks ---
    // Names are unique per package across all rule kinds; destinations (imports
    // and vendors both write into the tree) must not overlap.
    let names: Vec<(&str, &str, &str)> = imports
        .iter()
        .map(|i| (i.package.as_str(), i.name.as_str(), i.label.as_str()))
        .chain(
            vendors
                .iter()
                .map(|v| (v.package.as_str(), v.name.as_str(), v.label.as_str())),
        )
        .chain(
            exports
                .iter()
                .map(|e| (e.package.as_str(), e.name.as_str(), e.label.as_str())),
        )
        .chain(
            harnesses
                .iter()
                .map(|h| (h.package.as_str(), h.name.as_str(), h.label.as_str())),
        )
        .chain(
            models
                .iter()
                .map(|m| (m.package.as_str(), m.name.as_str(), m.label.as_str())),
        )
        .chain(
            agents
                .iter()
                .map(|a| (a.package.as_str(), a.name.as_str(), a.label.as_str())),
        )
        .chain(
            prompt_templates
                .iter()
                .map(|p| (p.package.as_str(), p.name.as_str(), p.label.as_str())),
        )
        .collect();
    check_unique_names(&names, &mut errors);

    let dests: Vec<(&str, &str)> = imports
        .iter()
        .map(|i| (i.label.as_str(), i.dest.as_str()))
        .chain(vendors.iter().map(|v| (v.label.as_str(), v.dest.as_str())))
        .collect();
    validate::check_destination_overlap(&dests, &mut errors);

    if !errors.is_empty() || monorepo.is_none() {
        errors.sort();
        errors.dedup();
        return Err(errors);
    }

    Ok(Ir {
        monorepo: monorepo.expect("checked above"),
        imports,
        vendors,
        exports,
        harnesses,
        models,
        agents,
        prompt_templates,
    })
}

/// Resolve a Bazel-style label `target` (e.g. `//tools/harness:claude_code`)
/// against the set of `candidates` labels of the expected `kind` (`harness` /
/// `model`). Returns the matched label on success; on failure, pushes a clear
/// diagnostic prefixed with the referencing rule's `label` and returns `None`.
fn resolve_label<'a>(
    label: &str,
    kind: &str,
    target: &str,
    candidates: impl Iterator<Item = &'a str>,
    errors: &mut Vec<String>,
) -> Option<String> {
    for cand in candidates {
        if cand == target {
            return Some(cand.to_owned());
        }
    }
    errors.push(format!(
        "{label}: {kind} label `{target}` does not resolve to a {kind} rule"
    ));
    None
}

/// Whether `s` has the shape of a Bazel-style label `//pkg/path:name` (used to
/// shape-check `agent_transform` var values that reference a target/file). This
/// validates form only — it does not check the target exists.
fn is_label_shaped(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("//") else {
        return false;
    };
    match rest.split_once(':') {
        Some((pkg, name)) => {
            !name.is_empty()
                && !name.contains('/')
                && !s.chars().any(char::is_whitespace)
                && (pkg.is_empty()
                    || pkg
                        .split('/')
                        .all(|seg| !seg.is_empty() && seg != "." && seg != ".."))
        }
        None => false,
    }
}

/// Names must be unique per package across all rule kinds. `entries` are
/// `(package, name, label)`.
fn check_unique_names(entries: &[(&str, &str, &str)], errors: &mut Vec<String>) {
    let mut seen: Vec<(String, String)> = Vec::new();
    for &(package, name, label) in entries {
        if seen
            .iter()
            .any(|(p, n)| p.as_str() == package && n.as_str() == name)
        {
            errors.push(format!("duplicate rule name in package {package}: {label}"));
        } else {
            seen.push((package.to_string(), name.to_string()));
        }
    }
}

/// Lower a captured transform list into validated IR transforms.
///
/// Each transform's subtree-relative paths are validated (no escape/absolute/`..`
/// segments) via [`validate::check_rel_path`]. The result is reordered by
/// [`Phase`] — mirror-phase transforms first, then tip-phase — with source order
/// preserved within each phase (a stable partition), matching the engine's
/// "mirror runs before tip" contract. `label` prefixes diagnostics.
fn lower_transforms(
    label: &str,
    dir: &str,
    specs: &[TransformSpec],
    errors: &mut Vec<String>,
) -> Vec<Transform> {
    let mut transforms: Vec<Transform> = specs
        .iter()
        .map(|spec| lower_transform(label, dir, spec, errors))
        .collect();
    // Stable partition: mirror-phase entries keep their relative order, then tip.
    transforms.sort_by_key(|t| match t.phase() {
        Phase::Mirror => 0,
        Phase::Tip => 1,
    });
    transforms
}

/// Lower and validate one transform spec into an IR transform. `dir` is the
/// declaring package's monorepo-root-relative directory, used to anchor the
/// `apply_patch` file path (the only package-relative path in a transform; all
/// other transform paths are subtree-relative).
fn lower_transform(
    label: &str,
    dir: &str,
    spec: &TransformSpec,
    errors: &mut Vec<String>,
) -> Transform {
    match spec {
        TransformSpec::Replace {
            before,
            after,
            paths,
            regex,
        } => {
            for p in paths {
                validate::check_glob_path(label, "replace paths", p, errors);
            }
            if before.is_empty() {
                errors.push(format!("{label}: replace `before` is empty"));
            }
            Transform::Replace {
                before: before.clone(),
                after: after.clone(),
                paths: paths.clone(),
                regex: *regex,
            }
        }
        TransformSpec::Move { src, dst } => {
            validate::check_rel_path(label, "move src", src, errors);
            validate::check_rel_path(label, "move dst", dst, errors);
            Transform::Move {
                src: src.clone(),
                dst: dst.clone(),
            }
        }
        TransformSpec::Copy { src, dst } => {
            validate::check_rel_path(label, "copy src", src, errors);
            validate::check_rel_path(label, "copy dst", dst, errors);
            Transform::Copy {
                src: src.clone(),
                dst: dst.clone(),
            }
        }
        TransformSpec::RewriteMessage {
            before,
            after,
            regex,
            strip_trailers,
            add_trailers,
        } => {
            if before.is_some() != after.is_some() {
                errors.push(format!(
                    "{label}: rewrite_message requires `before` and `after` together"
                ));
            }
            Transform::RewriteMessage {
                before: before.clone(),
                after: after.clone(),
                regex: *regex,
                strip_trailers: strip_trailers.clone(),
                add_trailers: add_trailers.clone(),
            }
        }
        TransformSpec::ApplyPatch { file } => {
            validate::check_rel_path(label, "apply_patch file", file, errors);
            Transform::ApplyPatch {
                file: join_dir(dir, Some(file)),
            }
        }
        TransformSpec::AgentTransform {
            agent,
            prompt,
            paths,
        } => {
            for p in paths {
                validate::check_glob_path(label, "agent_transform paths", p, errors);
            }
            // `agent`/`prompt.template` are cross-rule label references resolved
            // in a post-pass (`resolve_transform_labels`) once agents and prompt
            // templates are lowered. Var *values* that look like labels are
            // shape-checked there too; file existence is execution-time.
            Transform::AgentTransform {
                agent: agent.clone(),
                prompt_template: prompt.template.clone(),
                vars: prompt.vars.clone(),
                paths: paths.clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests;
