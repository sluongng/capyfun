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
    /// Replay an external repository's commits into a monorepo path.
    Import(ImportArgs),
    /// Publish a monorepo path to a destination remote as a GitHub PR.
    Export(ExportArgs),
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
        Command::Import(args) => run_import(args),
        Command::Export(args) => run_export(args),
    }
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
    match std::env::var("CAPYFUN_GITHUB_BASE") {
        Ok(base) => format!("{}/{}", base.trim_end_matches('/'), slug),
        Err(_) => format!("https://github.com/{slug}.git"),
    }
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

    let outcome = capyfun::engine::import(&repo, &import.dest, origin_tip, branch_tip, &patches)?;

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
