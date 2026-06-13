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
    /// Replay an external repository's commits into a monorepo path.
    Import(ImportArgs),
    /// Publish a monorepo path to a destination remote as a GitHub PR.
    Export(ExportArgs),
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
        Command::Import(args) => run_import(args),
        Command::Export(args) => run_export(args),
    }
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
