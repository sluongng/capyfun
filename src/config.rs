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

use anyhow::{bail, Context, Result};
use serde::Serialize;
use starlark::any::ProvidesStaticType;
use starlark::environment::{FrozenModule, Globals, GlobalsBuilder, Module};
use starlark::eval::{Evaluator, FileLoader};
use starlark::starlark_module;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use starlark::values::FrozenHeapName;

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
}

impl Decl {
    /// Human-readable rule kind, for diagnostics.
    pub fn kind(&self) -> &'static str {
        match self {
            Decl::Monorepo(_) => "monorepo",
            Decl::Import(_) => "github_import",
            Decl::Export(_) => "github_export",
            Decl::GitRepo(_) => "git_repository",
        }
    }

    /// Label of the package that declared this rule, e.g. `//third_party/backend`.
    pub fn package(&self) -> &str {
        match self {
            Decl::Monorepo(d) => &d.package,
            Decl::Import(d) => &d.package,
            Decl::Export(d) => &d.package,
            Decl::GitRepo(d) => &d.package,
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
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::Import(ImportDecl {
            name,
            repo,
            git_ref: r#ref.to_owned(),
            into,
            patches: patches.items,
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
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval)?;
        let package = s.package.clone();
        s.record(Decl::Export(ExportDecl {
            name,
            from_path,
            repo,
            branch: branch.to_owned(),
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
