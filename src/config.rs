//! CapyFun Starlark config evaluation.
//!
//! Defines the typed builtins (`monorepo`, `github_import`, `github_export`) and
//! captures their calls into in-memory [`Decl`]s. Evaluation is pure: no Git or
//! network I/O happens here.
//!
//! Config follows a Bazel-shaped two-file structure:
//!
//! - **SRC files** (literally named `SRC`) *instantiate* rules. Each one is a
//!   package at its directory.
//! - **`.scl` libraries** define macros and constants, loaded via
//!   `load("//path/to/lib.scl", "sym")`. They never instantiate rules — a
//!   top-level builtin call in a library is an error.
//!
//! The root SRC declares the singleton `monorepo` and anchors `//` load paths.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use allocative::Allocative;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use starlark::any::ProvidesStaticType;
use starlark::environment::{FrozenModule, Globals, GlobalsBuilder, Module};
use starlark::eval::{Evaluator, FileLoader};
use starlark::syntax::{AstModule, Dialect};
use starlark::values::dict::DictRef;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use starlark::values::{starlark_value, NoSerialize, StarlarkValue, Value, ValueLike};
use starlark::values::{FrozenHeapName, Heap};
use starlark::{starlark_module, starlark_simple_value};

/// A single captured rule instantiation, attributed to its declaring package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum Decl {
    #[serde(rename = "monorepo")]
    Monorepo(MonorepoDecl),
    #[serde(rename = "github_import")]
    Import(ImportDecl),
    #[serde(rename = "github_export")]
    Export(ExportDecl),
    #[serde(rename = "git_repository")]
    GitRepo(GitRepoDecl),
    #[serde(rename = "harness")]
    Harness(HarnessDecl),
    #[serde(rename = "model")]
    Model(ModelDecl),
    #[serde(rename = "agent")]
    Agent(AgentDecl),
    #[serde(rename = "prompt_template")]
    PromptTemplate(PromptTemplateDecl),
    #[serde(rename = "on_issue")]
    OnIssue(OnIssueDecl),
}

impl Decl {
    /// Human-readable rule kind, for diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Decl::Monorepo(_) => "monorepo",
            Decl::Import(_) => "github_import",
            Decl::Export(_) => "github_export",
            Decl::GitRepo(_) => "git_repository",
            Decl::Harness(_) => "harness",
            Decl::Model(_) => "model",
            Decl::Agent(_) => "agent",
            Decl::PromptTemplate(_) => "prompt_template",
            Decl::OnIssue(_) => "on_issue",
        }
    }

    /// Label of the package that declared this rule, e.g. `//third_party/backend`.
    pub fn package(&self) -> &str {
        match self {
            Decl::Monorepo(d) => &d.package,
            Decl::Import(d) => &d.package,
            Decl::Export(d) => &d.package,
            Decl::GitRepo(d) => &d.package,
            Decl::Harness(d) => &d.package,
            Decl::Model(d) => &d.package,
            Decl::Agent(d) => &d.package,
            Decl::PromptTemplate(d) => &d.package,
            Decl::OnIssue(d) => &d.package,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonorepoDecl {
    pub name: String,
    pub default_branch: String,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportDecl {
    pub name: String,
    /// GitHub `owner/name` slug.
    pub repo: String,
    /// Tracked upstream ref, e.g. `refs/heads/main`.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Optional subpath within the declaring package; `None` means the package
    /// directory itself.
    pub into: Option<String>,
    /// Ordered patch files, relative to the declaring SRC file.
    pub patches: Vec<String>,
    /// Ordered transform pipeline (from `transforms = [...]`), in source order.
    /// Empty when none are declared, so existing configs serialize unchanged at
    /// the IR boundary (the field is `#[serde(default)]` on the way back in).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<TransformSpec>,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportDecl {
    pub name: String,
    /// Subpath within the declaring package to export; `None` means the package.
    pub from_path: Option<String>,
    /// GitHub `owner/name` slug of the destination.
    pub repo: String,
    /// Destination branch the PR targets.
    pub branch: String,
    /// Ordered transform pipeline (from `transforms = [...]`), in source order.
    /// On export only structural transforms apply (`replace`/`move`/`copy`/
    /// `rewrite_message`); they rewrite the exported subtree before it ships
    /// (e.g. scrubbing internal-only lines). Empty when none are declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transforms: Vec<TransformSpec>,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepoDecl {
    pub name: String,
    /// GitHub `owner/name` slug.
    pub repo: String,
    /// Exact commit SHA to vendor (the pin).
    pub commit: String,
    /// Optional subpath within the declaring package; `None` means the package.
    pub into: Option<String>,
    pub package: String,
}

/// A captured transform from a `transforms = [...]` list, in its raw (pre-IR)
/// form: paths/text exactly as written, before package-anchoring or static
/// validation (those happen in [`crate::ir`], producing
/// [`crate::transform::Transform`]).
///
/// This is the closed, typed vocabulary of imperative transforms. The generative
/// `agent_transform` is added by a separate integration as a new variant here
/// (carrying its label/prompt fields); the lowering in [`crate::ir`] maps each
/// variant to a [`crate::transform::Transform`], so grafting it on is mechanical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "transform", rename_all = "snake_case")]
pub enum TransformSpec {
    Replace {
        before: String,
        after: String,
        paths: Vec<String>,
        regex: bool,
    },
    Move {
        src: String,
        dst: String,
    },
    Copy {
        src: String,
        dst: String,
    },
    RewriteMessage {
        before: Option<String>,
        after: Option<String>,
        regex: bool,
        strip_trailers: Vec<String>,
        add_trailers: Vec<String>,
    },
    /// Apply a static unified-diff patch file (tip phase). The path is relative
    /// to the declaring SRC package, like the `patches = [...]` sugar.
    ApplyPatch { file: String },
    /// Run a coding agent over the imported subtree and capture its edits as a
    /// patch (tip phase). `agent` and the prompt's template are labels.
    AgentTransform {
        agent: String,
        prompt: PromptSpec,
        paths: Vec<String>,
    },
}

/// A bound prompt: a `prompt_template` target label plus call-site `vars`.
/// `vars` values are literal strings or `//`-anchored label references (e.g. a
/// file label); ordering is normalized (sorted by key) for determinism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptSpec {
    /// Label of the `prompt_template` rule this prompt binds.
    pub template: String,
    /// Ordered (key, value) vars bound at the call site.
    pub vars: Vec<(String, String)>,
}

impl TransformSpec {
    /// Human-readable constructor name, for diagnostics.
    fn kind(&self) -> &'static str {
        match self {
            TransformSpec::Replace { .. } => "replace",
            TransformSpec::Move { .. } => "move",
            TransformSpec::Copy { .. } => "copy",
            TransformSpec::RewriteMessage { .. } => "rewrite_message",
            TransformSpec::ApplyPatch { .. } => "apply_patch",
            TransformSpec::AgentTransform { .. } => "agent_transform",
        }
    }
}

/// A heap-allocated Starlark value carrying a bound [`PromptSpec`], returned by
/// the `template()` constructor and consumed by `agent_transform(prompt = ...)`.
/// Like [`TransformValue`], it holds only owned `'static` data so it survives
/// `freeze()` and may be built in a `.scl` library.
#[derive(Debug, Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative)]
pub struct PromptValue {
    #[allocative(skip)]
    spec: PromptSpec,
}

impl std::fmt::Display for PromptValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<prompt {}>", self.spec.template)
    }
}

starlark_simple_value!(PromptValue);

#[starlark_value(type = "prompt")]
impl<'v> StarlarkValue<'v> for PromptValue {}

impl PromptValue {
    /// Pull a [`PromptSpec`] out of a Starlark value, erroring if it is not a
    /// `template(...)` value.
    fn unpack(value: Value) -> Result<PromptSpec> {
        value
            .downcast_ref::<PromptValue>()
            .map(|p| p.spec.clone())
            .with_context(|| {
                format!(
                    "`prompt` is `{}`, not a template; use template(...)",
                    value.get_type()
                )
            })
    }
}

/// Unpack a Starlark `vars = {...}` dict into sorted `(key, value)` string pairs.
/// Both keys and values must be strings; ordering is normalized by key so the
/// lowered IR is deterministic regardless of literal order.
fn unpack_vars(value: Value) -> Result<Vec<(String, String)>> {
    let dict = DictRef::from_value(value).with_context(|| {
        format!("template `vars` is `{}`, not a dict", value.get_type())
    })?;
    let mut out = Vec::with_capacity(dict.len());
    for (k, v) in dict.iter() {
        let key = k
            .unpack_str()
            .with_context(|| format!("template var key `{k}` is not a string"))?;
        let val = v
            .unpack_str()
            .with_context(|| format!("template var `{key}` value `{v}` is not a string"))?;
        out.push((key.to_owned(), val.to_owned()));
    }
    out.sort();
    Ok(out)
}

/// A heap-allocated Starlark value carrying a [`TransformSpec`].
///
/// Transform constructors (`replace`, `move`, …) return one of these. It is a
/// "simple" value: it holds only owned `'static` data, so it is identical frozen
/// or unfrozen and survives `freeze()` unchanged — which is why a transform
/// constant may be defined in a `.scl` library and `load()`ed into an SRC file.
#[derive(Debug, Clone, PartialEq, Eq, ProvidesStaticType, NoSerialize, Allocative)]
pub struct TransformValue {
    #[allocative(skip)]
    spec: TransformSpec,
}

impl std::fmt::Display for TransformValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<transform {}>", self.spec.kind())
    }
}

starlark_simple_value!(TransformValue);

#[starlark_value(type = "transform")]
impl<'v> StarlarkValue<'v> for TransformValue {}

impl TransformValue {
    fn new(spec: TransformSpec) -> Self {
        TransformValue { spec }
    }

    /// Pull a [`TransformSpec`] out of a Starlark value, erroring if it is not a
    /// transform (so `transforms = [replace(...), "oops"]` is rejected with a
    /// clear message at evaluation time).
    fn unpack(value: Value, index: usize) -> Result<TransformSpec> {
        value
            .downcast_ref::<TransformValue>()
            .map(|t| t.spec.clone())
            .with_context(|| {
                format!(
                    "transforms[{index}] is `{}`, not a transform; \
                     use replace/move/copy/rewrite_message",
                    value.get_type()
                )
            })
    }
}

/// An agent harness runtime (`harness`), carrying its `plugins`/`skills` as
/// runfiles (references to `git_repository` targets by label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessDecl {
    pub name: String,
    /// Harness runtime kind, e.g. `claude_code` / `codex` / `antigravity` / `pi`.
    pub kind: String,
    /// Labels of `git_repository` plugin targets to bring in as runfiles.
    pub plugins: Vec<String>,
    /// Labels of `git_repository` skill targets to bring in as runfiles.
    pub skills: Vec<String>,
    pub package: String,
}

/// An LLM (`model`): a provider + id, plus an optional credential *reference*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelDecl {
    pub name: String,
    /// Provider, e.g. `anthropic` / `openai` / `google` / `nebius`.
    pub provider: String,
    /// Provider-specific model id.
    pub id: String,
    /// Optional credential reference (e.g. `env:NAME`); never a secret value.
    pub credential: Option<String>,
    pub package: String,
}

/// An agent (`agent`): pairs a `harness` with a `model`, both by label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentDecl {
    pub name: String,
    /// Label of the `harness` rule this agent runs on.
    pub harness: String,
    /// Label of the `model` rule this agent drives.
    pub model: String,
    pub package: String,
}

/// A prompt template (`prompt_template`): wraps a `.tmpl` file by label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptTemplateDecl {
    pub name: String,
    /// `.tmpl` file path relative to the declaring SRC package.
    pub src: String,
    pub package: String,
}

/// An issue-triggered reaction (`on_issue`): when a matching `issues` webhook
/// fires on `repo`, run `agent` with `prompt` over a checkout of the repo and
/// open a PR. This is a *reaction* rule — the generative counterpart to
/// import/export, declared in an SRC file like any other rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OnIssueDecl {
    pub name: String,
    /// GitHub `owner/name` slug the App is installed on (the issue's repo and the
    /// PR destination).
    pub repo: String,
    /// Optional issue action filter (e.g. `opened` / `labeled`); `None` matches
    /// the default action set (opened + labeled).
    pub action: Option<String>,
    /// Optional issue-label filter; `None` matches any label.
    pub label: Option<String>,
    /// Label of the `agent` rule to run.
    pub agent: String,
    /// The bound prompt (`template(...)`) the agent runs with.
    pub prompt: PromptSpec,
    pub package: String,
}

/// The captured result of evaluating every SRC file under a monorepo root.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct RawConfig {
    pub decls: Vec<Decl>,
}

/// Mutable state threaded through one module evaluation via `eval.extra`.
#[derive(Debug, ProvidesStaticType)]
struct EvalState {
    /// True while evaluating a `.scl` library; builtins error in this mode.
    is_library: bool,
    /// Label of the package being evaluated, e.g. `//third_party/backend`.
    package: String,
    decls: RefCell<Vec<Decl>>,
}

impl EvalState {
    fn record(&self, decl: Decl) -> Result<()> {
        if self.is_library {
            bail!(
                "rule `{}` cannot be instantiated in a .scl library; \
                 instantiate rules in SRC files (libraries define macros only)",
                decl.kind()
            );
        }
        self.decls.borrow_mut().push(decl);
        Ok(())
    }
}

/// Read the [`EvalState`] back out of the evaluator.
fn state<'a>(eval: &'a Evaluator) -> Result<&'a EvalState> {
    eval.extra
        .context("internal: evaluator has no CapyFun state")?
        .downcast_ref::<EvalState>()
        .context("internal: evaluator extra was not EvalState")
}

#[starlark_module]
fn capyfun_globals(builder: &mut GlobalsBuilder) {
    /// Declare the monorepo singleton. Allowed only in the root SRC file.
    fn monorepo(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] default_branch: String,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::Monorepo(MonorepoDecl {
            name,
            default_branch,
            package,
        }))?;
        Ok(NoneType)
    }

    /// Import a GitHub repository into the declaring package's directory.
    fn github_import(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] repo: String,
        #[starlark(require = named, default = "refs/heads/main")] r#ref: &str,
        #[starlark(require = named)] into: Option<String>,
        #[starlark(require = named, default = UnpackList::default())] patches: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] transforms: UnpackList<Value>,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        let transforms = transforms
            .items
            .into_iter()
            .enumerate()
            .map(|(i, v)| TransformValue::unpack(v, i))
            .collect::<Result<Vec<_>>>()?;
        s.record(Decl::Import(ImportDecl {
            name,
            repo,
            git_ref: r#ref.to_owned(),
            into,
            patches: patches.items,
            transforms,
            package,
        }))?;
        Ok(NoneType)
    }

    /// Vendor a pinned snapshot of a GitHub repo into the declaring package.
    fn git_repository(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] repo: String,
        #[starlark(require = named)] commit: String,
        #[starlark(require = named)] into: Option<String>,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::GitRepo(GitRepoDecl {
            name,
            repo,
            commit,
            into,
            package,
        }))?;
        Ok(NoneType)
    }

    /// Export the declaring package's directory to a GitHub repo via a PR.
    fn github_export(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] repo: String,
        #[starlark(require = named, default = "main")] branch: &str,
        #[starlark(require = named)] from_path: Option<String>,
        #[starlark(require = named, default = UnpackList::default())] transforms: UnpackList<Value>,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        let transforms = transforms
            .items
            .into_iter()
            .enumerate()
            .map(|(i, v)| TransformValue::unpack(v, i))
            .collect::<Result<Vec<_>>>()?;
        s.record(Decl::Export(ExportDecl {
            name,
            from_path,
            repo,
            branch: branch.to_owned(),
            transforms,
            package,
        }))?;
        Ok(NoneType)
    }

    // --- transform value constructors (pure; usable anywhere, incl. libraries) ---
    //
    // These return a typed `transform` value and record nothing, so the
    // "no rule instantiation in a library" guard does not apply: a `.scl`
    // library may define reusable transform constants. They attach to an import
    // via `github_import(..., transforms = [...])`.

    /// A sed-like search/replace across blob contents of files matching `paths`.
    fn replace<'v>(
        #[starlark(require = named)] before: String,
        #[starlark(require = named)] after: String,
        #[starlark(require = named, default = UnpackList::default())] paths: UnpackList<String>,
        #[starlark(require = named, default = false)] regex: bool,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(TransformValue::new(TransformSpec::Replace {
            before,
            after,
            paths: paths.items,
            regex,
        })))
    }

    /// Relocate a file or directory within the imported subtree.
    fn r#move<'v>(
        #[starlark(require = named)] src: String,
        #[starlark(require = named)] dst: String,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(TransformValue::new(TransformSpec::Move { src, dst })))
    }

    /// Duplicate a file or directory within the imported subtree.
    fn copy<'v>(
        #[starlark(require = named)] src: String,
        #[starlark(require = named)] dst: String,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(TransformValue::new(TransformSpec::Copy { src, dst })))
    }

    /// Rewrite each commit message: optional body substitution plus trailer
    /// strip/add. The engine still always preserves the `CapyFun-Origin`/
    /// `CapyFun-Import` trailers.
    fn rewrite_message<'v>(
        #[starlark(require = named)] before: Option<String>,
        #[starlark(require = named)] after: Option<String>,
        #[starlark(require = named, default = false)] regex: bool,
        #[starlark(require = named, default = UnpackList::default())] strip_trailers: UnpackList<
            String,
        >,
        #[starlark(require = named, default = UnpackList::default())] add_trailers: UnpackList<
            String,
        >,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(
            heap.alloc(TransformValue::new(TransformSpec::RewriteMessage {
                before,
                after,
                regex,
                strip_trailers: strip_trailers.items,
                add_trailers: add_trailers.items,
            })),
        )
    }

    /// Apply a static unified-diff patch file (tip phase). The path is relative
    /// to the declaring SRC package.
    fn apply_patch<'v>(
        #[starlark(require = pos)] file: String,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(TransformValue::new(TransformSpec::ApplyPatch { file })))
    }

    /// Bind a `prompt_template` target (by label) to call-site `vars`, producing
    /// a prompt value for `agent_transform(prompt = ...)`. Pure: records nothing.
    fn template<'v>(
        #[starlark(require = pos)] prompt_template: String,
        #[starlark(require = named)] vars: Option<Value<'v>>,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        let vars = match vars {
            Some(v) => unpack_vars(v)?,
            None => Vec::new(),
        };
        Ok(heap.alloc(PromptValue {
            spec: PromptSpec {
                template: prompt_template,
                vars,
            },
        }))
    }

    /// Run a coding agent over the imported subtree and capture its edits as a
    /// patch (tip phase). `agent` is an `agent` rule label; `prompt` is a
    /// `template(...)` value; `paths` optionally scopes the agent's view.
    fn agent_transform<'v>(
        #[starlark(require = named)] agent: String,
        #[starlark(require = named)] prompt: Value<'v>,
        #[starlark(require = named, default = UnpackList::default())] paths: UnpackList<String>,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        let prompt = PromptValue::unpack(prompt)?;
        Ok(
            heap.alloc(TransformValue::new(TransformSpec::AgentTransform {
                agent,
                prompt,
                paths: paths.items,
            })),
        )
    }

    /// Declare an agent harness runtime. `kind` selects the runtime; `plugins`
    /// and `skills` are labels of `git_repository` targets brought in as runfiles.
    fn harness(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] kind: String,
        #[starlark(require = named, default = UnpackList::default())] plugins: UnpackList<String>,
        #[starlark(require = named, default = UnpackList::default())] skills: UnpackList<String>,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::Harness(HarnessDecl {
            name,
            kind,
            plugins: plugins.items,
            skills: skills.items,
            package,
        }))?;
        Ok(NoneType)
    }

    /// Declare an LLM. `credential` is an optional `env:NAME` reference (never a
    /// secret value); with none, the provider's conventional env var is used.
    fn model(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] provider: String,
        #[starlark(require = named)] id: String,
        #[starlark(require = named)] credential: Option<String>,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::Model(ModelDecl {
            name,
            provider,
            id,
            credential,
            package,
        }))?;
        Ok(NoneType)
    }

    /// Declare an agent pairing a `harness` and a `model`, both by label.
    fn agent(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] harness: String,
        #[starlark(require = named)] model: String,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::Agent(AgentDecl {
            name,
            harness,
            model,
            package,
        }))?;
        Ok(NoneType)
    }

    /// Declare a prompt template wrapping a `.tmpl` file (relative to the package).
    fn prompt_template(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] src: String,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::PromptTemplate(PromptTemplateDecl {
            name,
            src,
            package,
        }))?;
        Ok(NoneType)
    }

    /// React to GitHub `issues` events on `repo`: when the (optional) `action`
    /// and `label` filters match, run `agent` with `prompt` over a checkout of
    /// the repo and open a PR. `prompt` is a `template(...)` value, like
    /// `agent_transform`.
    fn on_issue<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] repo: String,
        #[starlark(require = named)] agent: String,
        #[starlark(require = named)] prompt: Value<'v>,
        #[starlark(require = named)] action: Option<String>,
        #[starlark(require = named)] label: Option<String>,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let prompt = PromptValue::unpack(prompt)?;
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::OnIssue(OnIssueDecl {
            name,
            repo,
            action,
            label,
            agent,
            prompt,
            package,
        }))?;
        Ok(NoneType)
    }
}

/// Build the CapyFun globals (standard Starlark + our typed builtins).
fn globals() -> Globals {
    GlobalsBuilder::standard().with(capyfun_globals).build()
}

/// Resolve a `//`-anchored load path to a filesystem path under `root`.
fn resolve_load(root: &Path, module_id: &str) -> Result<PathBuf> {
    let rel = module_id.strip_prefix("//").with_context(|| {
        format!("load path `{module_id}` must be `//`-anchored from the monorepo root")
    })?;
    if rel
        .split('/')
        .any(|c| c == ".." || c == "." || c.is_empty())
    {
        bail!("load path `{module_id}` must not contain empty, `.`, or `..` segments");
    }
    Ok(root.join(rel))
}

/// A [`FileLoader`] that resolves `//`-anchored library paths under the monorepo
/// root, evaluating each library in library mode and caching the result.
struct CapyLoader<'a> {
    root: &'a Path,
    globals: &'a Globals,
    cache: RefCell<HashMap<String, FrozenModule>>,
}

impl<'a> CapyLoader<'a> {
    fn new(root: &'a Path, globals: &'a Globals) -> Self {
        CapyLoader {
            root,
            globals,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Evaluate a `.scl` library file into a frozen module (library mode).
    fn eval_library(&self, abs: &Path, module_id: &str) -> Result<FrozenModule> {
        let src = fs::read_to_string(abs)
            .with_context(|| format!("reading library `{module_id}` at {}", abs.display()))?;
        let ast = AstModule::parse(module_id, src, &Dialect::Standard)
            .map_err(|e| e.into_anyhow())
            .with_context(|| format!("parsing library `{module_id}`"))?;

        let state = EvalState {
            is_library: true,
            package: module_id.to_owned(),
            decls: RefCell::new(Vec::new()),
        };

        Module::with_temp_heap(|module| {
            {
                let mut eval = Evaluator::new(&module);
                eval.set_loader(self);
                eval.extra = Some(&state);
                eval.eval_module(ast, self.globals)
                    .map_err(|e| e.into_anyhow())
                    .with_context(|| format!("evaluating library `{module_id}`"))?;
            }
            let frozen = module
                .freeze_named(FrozenHeapName::User(Box::new(module_id.to_owned())))
                .map_err(anyhow::Error::from)
                .with_context(|| format!("freezing library `{module_id}`"))?;
            Ok(frozen)
        })
    }
}

impl FileLoader for CapyLoader<'_> {
    fn load(&self, module_id: &str) -> starlark::Result<FrozenModule> {
        if let Some(m) = self.cache.borrow().get(module_id) {
            return Ok(m.clone());
        }
        let abs = resolve_load(self.root, module_id).map_err(starlark::Error::new_other)?;
        let module = self
            .eval_library(&abs, module_id)
            .map_err(starlark::Error::new_other)?;
        self.cache
            .borrow_mut()
            .insert(module_id.to_owned(), module.clone());
        Ok(module)
    }
}

/// Compute a package label (e.g. `//third_party/backend`, or `//` for the root)
/// for an SRC file under `root`.
fn package_label(root: &Path, src_file: &Path) -> Result<String> {
    let dir = src_file
        .parent()
        .with_context(|| format!("SRC file {} has no parent", src_file.display()))?;
    let rel = dir.strip_prefix(root).with_context(|| {
        format!(
            "SRC file {} is not under root {}",
            src_file.display(),
            root.display()
        )
    })?;
    if rel.as_os_str().is_empty() {
        Ok("//".to_owned())
    } else {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Ok(format!("//{}", parts.join("/")))
    }
}

/// Evaluate a single SRC file, returning the rules it declared.
fn eval_src(root: &Path, src_file: &Path, globals: &Globals) -> Result<Vec<Decl>> {
    let package = package_label(root, src_file)?;
    let src = fs::read_to_string(src_file)
        .with_context(|| format!("reading SRC file {}", src_file.display()))?;
    let ast = AstModule::parse(&package, src, &Dialect::Standard)
        .map_err(|e| e.into_anyhow())
        .with_context(|| format!("parsing SRC `{package}`"))?;

    let loader = CapyLoader::new(root, globals);
    let state = EvalState {
        is_library: false,
        package: package.clone(),
        decls: RefCell::new(Vec::new()),
    };

    Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.set_loader(&loader);
        eval.extra = Some(&state);
        eval.eval_module(ast, globals)
            .map_err(|e| e.into_anyhow())
            .with_context(|| format!("evaluating SRC `{package}`"))?;
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(state.decls.into_inner())
}

/// Recursively discover every file named `SRC` under `root`.
///
/// Skips `.git` and `target` directories. Returns paths sorted for determinism.
pub fn discover_src_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .with_context(|| format!("reading directory {}", dir.display()))?
            .collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if name == ".git" || name == "target" {
                    continue;
                }
                walk(&path, out)?;
            } else if name == "SRC" {
                out.push(path);
            }
        }
        Ok(())
    }

    let mut out = Vec::new();
    walk(root, &mut out)?;
    Ok(out)
}

/// Discover and evaluate every SRC file under `root` into a [`RawConfig`].
pub fn evaluate(root: &Path) -> Result<RawConfig> {
    let globals = globals();
    let src_files = discover_src_files(root)?;
    let mut decls = Vec::new();
    for src_file in &src_files {
        decls.extend(eval_src(root, src_file, &globals)?);
    }
    Ok(RawConfig { decls })
}

#[cfg(test)]
mod tests;
