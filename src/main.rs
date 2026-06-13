//! `capyfun` CLI entrypoint.

use std::path::PathBuf;

use anyhow::{bail, Result};
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
        Command::Import(args) => run_import(args),
        Command::Export(args) => run_export(args),
    }
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

fn run_import(args: ImportArgs) -> Result<()> {
    bail!(
        "import '{}' (root {}) is not implemented yet (see docs/plans/import-roadmap.md, M4-M7)",
        args.label,
        args.root.display()
    );
}

fn run_export(args: ExportArgs) -> Result<()> {
    bail!(
        "export '{}' (root {}) is not implemented yet (deferred, see docs/plans/import-roadmap.md, M8)",
        args.label,
        args.root.display()
    );
}
