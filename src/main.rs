//! `capyfun` CLI entrypoint.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

/// CapyFun: import code into and export code out of the TinyTree monorepo.
#[derive(Debug, Parser)]
#[command(name = "capyfun", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Discover and evaluate SRC files, listing the captured rules.
    Config(ConfigArgs),
    /// Evaluate, lower to IR, and statically validate; print the IR as JSON.
    Check(ConfigArgs),
    /// Scaffold import SRC files from a Go module's go.mod / go.sum.
    GenGo(GenGoArgs),
    /// Scaffold git_repository snapshot SRC files from a Cargo.toml / Cargo.lock.
    GenCargo(GenSnapshotArgs),
    /// Scaffold a git_repository snapshot SRC from a package.json / package-lock.json.
    GenNpm(GenSnapshotArgs),
    /// Replay an external repository's commits into a monorepo path.
    Import(ImportArgs),
    /// Vendor a pinned snapshot of a `git_repository` rule into its package.
    Vendor(ImportArgs),
    /// Publish a monorepo path to a destination remote as a GitHub PR.
    Export(ExportArgs),
    /// Run the automation server: poll GH Archive and host the webhook endpoint.
    Serve(ServeArgs),
    /// Run a coding-agent harness over a prompt (proof of the agent_transform path).
    AgentRun(AgentRunArgs),
}

#[derive(Debug, clap::Args)]
struct AgentRunArgs {
    /// Prompt to send to the agent. If omitted, read from stdin.
    #[arg(long)]
    prompt: Option<String>,
    /// Harness runtime to drive: claude_code, codex, antigravity, or pi.
    #[arg(long, default_value = "claude_code")]
    harness: String,
    /// Model provider (selects the conventional credential env var).
    #[arg(long, default_value = "anthropic")]
    provider: String,
    /// Model id passed to the harness (e.g. claude-opus-4-8). Optional.
    #[arg(long)]
    model: Option<String>,
    /// Credential reference, e.g. `env:ANTHROPIC_API_KEY`. Defaults to the
    /// provider's conventional env var; claude_code falls through to CLI login.
    #[arg(long)]
    credential: Option<String>,
    /// OpenAI-compatible base URL for the `pi` harness (defaults per provider).
    #[arg(long)]
    base_url: Option<String>,
}

#[derive(Debug, clap::Args)]
struct ServeArgs {
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Address for the webhook/health HTTP endpoint.
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    /// Seconds between GH Archive poll cycles.
    #[arg(long, default_value_t = 3600)]
    interval_secs: u64,
    /// Run a single poll cycle and exit (no HTTP server).
    #[arg(long)]
    once: bool,
}

#[derive(Debug, clap::Args)]
struct ConfigArgs {
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
}

#[derive(Debug, clap::Args)]
struct GenGoArgs {
    /// Monorepo root where SRC files are written.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Path to go.mod (default: <root>/go.mod).
    #[arg(long)]
    go_mod: Option<PathBuf>,
    /// Third-party prefix for generated packages.
    #[arg(long, default_value = "third_party")]
    prefix: String,
    /// Also generate imports for indirect dependencies.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, clap::Args)]
struct GenSnapshotArgs {
    /// Monorepo root where SRC files are written.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Path to the manifest (default: <root>/Cargo.toml or <root>/package.json).
    #[arg(long)]
    manifest: Option<PathBuf>,
    /// Third-party prefix for generated packages.
    #[arg(long)]
    prefix: Option<String>,
}

#[derive(Debug, clap::Args)]
struct ImportArgs {
    /// Label of the `github_import` rule to run, e.g. `//third_party/backend:backend`.
    label: String,
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
}

#[derive(Debug, clap::Args)]
struct ExportArgs {
    /// Label of the `github_export` rule to run, e.g. `//sdk/go:public-go-sdk`.
    label: String,
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Config(args) => run_config(args),
        Command::Check(args) => run_check(args),
        Command::GenGo(args) => run_gen_go(args),
        Command::GenCargo(args) => run_gen_cargo(args),
        Command::GenNpm(args) => run_gen_npm(args),
        Command::Import(args) => run_import(args),
        Command::Vendor(args) => run_vendor(args),
        Command::Export(args) => run_export(args),
        Command::Serve(args) => run_serve(args),
        Command::AgentRun(args) => run_agent_run(args),
    }
}

fn run_agent_run(args: AgentRunArgs) -> Result<()> {
    use std::io::Read;

    let harness = capyfun::agent::HarnessKind::parse(&args.harness)?;

    let prompt = match args.prompt {
        Some(p) => p,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading prompt from stdin")?;
            let buf = buf.trim().to_string();
            if buf.is_empty() {
                bail!("no prompt given (pass --prompt or pipe text on stdin)");
            }
            buf
        }
    };

    let run = capyfun::agent::AgentRun {
        harness,
        provider: args.provider,
        model_id: args.model,
        credential: args.credential,
        base_url: args.base_url,
        prompt,
    };

    let out = capyfun::agent::run_agent(&run)?;
    match &out.credential {
        capyfun::agent::Credential::Resolved { var } => {
            eprintln!("credential: resolved from ${var}");
        }
        capyfun::agent::Credential::Ambient => {
            eprintln!(
                "credential: none set; using the {} harness's own login",
                harness.as_str()
            );
        }
    }
    print!("{}", out.stdout);
    if !out.stdout.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn run_serve(args: ServeArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;
    capyfun::server::serve(
        &ir,
        &args.addr,
        std::time::Duration::from_secs(args.interval_secs),
        args.once,
    )
}

fn run_vendor(args: ImportArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;

    let vendor = ir
        .vendors
        .iter()
        .find(|v| v.label == args.label)
        .ok_or_else(|| {
            let labels: Vec<&str> = ir.vendors.iter().map(|v| v.label.as_str()).collect();
            anyhow::anyhow!(
                "no git_repository labeled `{}` (available: {})",
                args.label,
                if labels.is_empty() {
                    "none".into()
                } else {
                    labels.join(", ")
                }
            )
        })?;

    let repo = git2::Repository::open(&args.root)
        .with_context(|| format!("opening monorepo at {}", args.root.display()))?;
    let branch_ref = format!("refs/heads/{}", ir.monorepo.default_branch);
    let branch_tip = repo
        .find_reference(&branch_ref)
        .ok()
        .and_then(|r| r.target());

    let url = origin_url(&vendor.repo);
    let commit = capyfun::engine::fetch_commit(&repo, &url, &vendor.commit)?;
    let outcome =
        capyfun::engine::vendor_snapshot(&repo, &vendor.dest, &vendor.repo, commit, branch_tip)?;

    match outcome.head {
        Some(head) if Some(head) != branch_tip => {
            repo.reference(
                &branch_ref,
                head,
                true,
                &format!("capyfun vendor {}", vendor.label),
            )?;
            println!(
                "vendored {}@{} into {}; {} now {}",
                vendor.repo, vendor.commit, vendor.dest, branch_ref, head
            );
        }
        _ => println!("{} is already vendored at {}", vendor.label, vendor.commit),
    }
    Ok(())
}

fn run_gen_go(args: GenGoArgs) -> Result<()> {
    use capyfun::gomod;

    let go_mod_path = args.go_mod.unwrap_or_else(|| args.root.join("go.mod"));
    let content = std::fs::read_to_string(&go_mod_path)
        .with_context(|| format!("reading {}", go_mod_path.display()))?;
    let requires = gomod::parse_go_mod(&content);

    // Cross-check against go.sum when present.
    let go_sum_path = args.root.join("go.sum");
    let go_sum = std::fs::read_to_string(&go_sum_path)
        .ok()
        .map(|c| gomod::parse_go_sum(&c));

    let (imports, skipped) = gomod::plan_imports(&requires, &args.prefix, args.all);

    let mut written = 0;
    for gi in &imports {
        if let Some(sum) = &go_sum {
            if !sum.iter().any(|(p, _)| {
                p == &gi.module_path || p.starts_with(&format!("{}/", gi.module_path))
            }) {
                println!("warning: {} not found in go.sum", gi.module_path);
            }
        }
        let dir = args.root.join(&gi.package_dir);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(dir.join("SRC"), gomod::render_src(gi))
            .with_context(|| format!("writing {}/SRC", gi.package_dir))?;
        println!(
            "wrote {}/SRC  ({} @ {})",
            gi.package_dir, gi.slug, gi.git_ref
        );
        written += 1;
    }

    for s in &skipped {
        println!("skipped {s}");
    }
    println!(
        "\ngenerated {written} import(s); run `capyfun import //<pkg>:<name> --root {}`",
        args.root.display()
    );
    Ok(())
}

/// Resolve a planned package to a pinned GitHub snapshot: find its GitHub slug
/// (from a git source if present, else the registry) and resolve its version tag
/// to a commit SHA via `ls-remote`. `repo_lookup` queries the ecosystem's
/// registry for a package's recorded repository URL.
fn resolve_snapshot(
    name: &str,
    version: &str,
    git_slug: Option<(String, String)>,
    git_sha: Option<String>,
    repo_lookup: impl FnOnce(&str) -> Result<Option<String>>,
) -> Result<capyfun::vendorgen::Vendored> {
    use capyfun::vendorgen::{ls_remote, parse_github_slug, pick_commit, tag_candidates};

    let (owner, repo) = match git_slug {
        Some(s) => s,
        None => {
            let url = repo_lookup(name)?
                .ok_or_else(|| anyhow::anyhow!("no repository URL recorded for {name}"))?;
            parse_github_slug(&url)
                .ok_or_else(|| anyhow::anyhow!("{name} repository {url} is not on github.com"))?
        }
    };
    let slug = format!("{owner}/{repo}");

    // A git-source lock entry already pins an exact commit; skip the network.
    if let Some(sha) = git_sha {
        return Ok(capyfun::vendorgen::Vendored {
            name: name.to_owned(),
            slug,
            version: version.to_owned(),
            commit: sha,
            tag: version.to_owned(),
        });
    }

    let refs = ls_remote(&slug)?;
    let (tag, commit) = pick_commit(&refs, &tag_candidates(name, version)).ok_or_else(|| {
        anyhow::anyhow!(
            "no tag for {name} {version} found in {slug} (tried {:?})",
            tag_candidates(name, version)
        )
    })?;
    Ok(capyfun::vendorgen::Vendored {
        name: name.to_owned(),
        slug,
        version: version.to_owned(),
        commit,
        tag,
    })
}

fn run_gen_cargo(args: GenSnapshotArgs) -> Result<()> {
    use capyfun::{cargo, vendorgen};

    let prefix = args.prefix.as_deref().unwrap_or("third_party/rust");
    let manifest_path = args
        .manifest
        .unwrap_or_else(|| args.root.join("Cargo.toml"));
    let content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let deps = cargo::parse_manifest(&content);

    let lock = std::fs::read_to_string(args.root.join("Cargo.lock"))
        .ok()
        .map(|c| cargo::parse_lock(&c))
        .unwrap_or_default();

    let planned = cargo::plan(&deps, &lock, prefix);

    let mut written = 0;
    let mut skipped = Vec::new();
    for p in &planned {
        let git_slug = cargo::slug_from_git(p);
        match resolve_snapshot(
            &p.name,
            &p.version,
            git_slug,
            p.git_sha.clone(),
            vendorgen::crates_io_repo,
        ) {
            Ok(v) => {
                let dir = args.root.join(&p.package_dir);
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
                std::fs::write(dir.join("SRC"), cargo::render_src(&v))
                    .with_context(|| format!("writing {}/SRC", p.package_dir))?;
                println!("wrote {}/SRC  ({} @ {})", p.package_dir, v.slug, v.tag);
                written += 1;
            }
            Err(e) => skipped.push(format!("{} {} ({e})", p.name, p.version)),
        }
    }
    for s in &skipped {
        println!("skipped {s}");
    }
    println!(
        "\ngenerated {written} snapshot(s); run `capyfun vendor //<pkg>:<name> --root {}`",
        args.root.display()
    );
    Ok(())
}

fn run_gen_npm(args: GenSnapshotArgs) -> Result<()> {
    use capyfun::{npm, vendorgen};

    let prefix = args.prefix.as_deref().unwrap_or("third_party/js");
    let manifest_path = args
        .manifest
        .unwrap_or_else(|| args.root.join("package.json"));
    let content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let deps = npm::parse_manifest(&content)?;

    let lock = std::fs::read_to_string(args.root.join("package-lock.json"))
        .ok()
        .map(|c| npm::parse_lock(&c))
        .transpose()?
        .unwrap_or_default();

    let planned = npm::plan(&deps, &lock);

    let mut resolved = Vec::new();
    let mut skipped = Vec::new();
    for p in &planned {
        match resolve_snapshot(&p.name, &p.version, None, None, vendorgen::npm_repo) {
            Ok(v) => {
                println!("resolved {} -> {} @ {}", p.name, v.slug, v.tag);
                resolved.push((v, p.into.clone()));
            }
            Err(e) => skipped.push(format!("{} {} ({e})", p.name, p.version)),
        }
    }

    if !resolved.is_empty() {
        let dir = args.root.join(prefix);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(dir.join("SRC"), npm::render_src(&resolved))
            .with_context(|| format!("writing {prefix}/SRC"))?;
        println!("wrote {prefix}/SRC  ({} target(s))", resolved.len());
    }
    for s in &skipped {
        println!("skipped {s}");
    }
    println!(
        "\ngenerated {} snapshot target(s); run `capyfun vendor //{prefix}:<name> --root {}`",
        resolved.len(),
        args.root.display()
    );
    Ok(())
}

fn run_check(args: ConfigArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    match capyfun::ir::compile(&raw) {
        Ok(ir) => {
            println!("{}", serde_json::to_string_pretty(&ir)?);
            Ok(())
        }
        Err(diags) => {
            bail!("config is invalid:\n  {}", diags.join("\n  "));
        }
    }
}

fn run_config(args: ConfigArgs) -> Result<()> {
    let cfg = capyfun::config::evaluate(&args.root)?;
    if cfg.decls.is_empty() {
        println!("no rules found under {}", args.root.display());
        return Ok(());
    }
    for decl in &cfg.decls {
        match decl {
            capyfun::config::Decl::Monorepo(m) => {
                println!(
                    "{}  monorepo {}  default_branch={}",
                    m.package, m.name, m.default_branch
                );
            }
            capyfun::config::Decl::Import(i) => {
                let into = i.into.as_deref().unwrap_or("<package>");
                println!(
                    "{}:{}  github_import repo={} ref={} into={} patches={}",
                    i.package,
                    i.name,
                    i.repo,
                    i.git_ref,
                    into,
                    i.patches.len()
                );
            }
            capyfun::config::Decl::Export(e) => {
                let from = e.from_path.as_deref().unwrap_or("<package>");
                println!(
                    "{}:{}  github_export repo={} branch={} from={}",
                    e.package, e.name, e.repo, e.branch, from
                );
            }
            capyfun::config::Decl::GitRepo(g) => {
                let into = g.into.as_deref().unwrap_or("<package>");
                println!(
                    "{}:{}  git_repository repo={} commit={} into={}",
                    g.package, g.name, g.repo, g.commit, into
                );
            }
        }
    }
    Ok(())
}

/// Resolve a GitHub `owner/name` slug to a fetchable URL.
///
/// `CAPYFUN_GITHUB_BASE` overrides the GitHub base (used to point imports at
/// local bare repositories in hermetic tests/demos); otherwise the public
/// GitHub HTTPS URL is used.
fn origin_url(slug: &str) -> String {
    capyfun::vendorgen::github_url(slug)
}

fn run_import(args: ImportArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;

    let import = ir
        .imports
        .iter()
        .find(|i| i.label == args.label)
        .ok_or_else(|| {
            let labels: Vec<&str> = ir.imports.iter().map(|i| i.label.as_str()).collect();
            anyhow::anyhow!(
                "no github_import labeled `{}` (available: {})",
                args.label,
                if labels.is_empty() {
                    "none".into()
                } else {
                    labels.join(", ")
                }
            )
        })?;

    let repo = git2::Repository::open(&args.root)
        .with_context(|| format!("opening monorepo at {}", args.root.display()))?;

    // Current tip of the monorepo branch we import onto.
    let branch_ref = format!("refs/heads/{}", ir.monorepo.default_branch);
    let branch_tip = repo
        .find_reference(&branch_ref)
        .ok()
        .and_then(|r| r.target());

    // Fetch the origin ref into the monorepo's object store.
    let url = origin_url(&import.repo);
    let origin_tip = capyfun::engine::fetch_commit(&repo, &url, &import.git_ref)?;

    // Read the patch series from the working tree.
    let patches = import
        .patches
        .iter()
        .map(|p| {
            let bytes =
                std::fs::read(args.root.join(p)).with_context(|| format!("reading patch {p}"))?;
            Ok(capyfun::engine::PatchFile {
                label: p.clone(),
                bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let outcome = capyfun::engine::import(
        &repo,
        &import.dest,
        origin_tip,
        branch_tip,
        &import.transforms,
        &patches,
    )?;

    match outcome.head {
        Some(head) if Some(head) != branch_tip => {
            repo.reference(
                &branch_ref,
                head,
                true,
                &format!("capyfun import {}", import.label),
            )?;
            println!(
                "imported {} commit(s) for {} into {}; {} now {}",
                outcome.imported, import.label, import.dest, branch_ref, head
            );
        }
        _ => println!("{} is already up to date", import.label),
    }
    Ok(())
}

fn run_export(args: ExportArgs) -> Result<()> {
    bail!(
        "export '{}' (root {}) is not implemented yet (deferred, see docs/plans/import-roadmap.md, M8)",
        args.label,
        args.root.display()
    );
}
