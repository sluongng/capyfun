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
    /// Dry-run reconcile: report each target's desired-vs-actual sync state.
    Status(StatusArgs),
    /// Reconcile targets: run the import/vendor/export needed to converge.
    Reconcile(ReconcileArgs),
    /// Run the automation server: poll GH Archive and host the webhook endpoint.
    Serve(ServeArgs),
    /// Run an `on_issue` reaction from a saved GitHub `issues` webhook payload.
    React(ReactArgs),
    /// Run a coding-agent harness over a prompt (proof of the agent_transform path).
    AgentRun(AgentRunArgs),
}

#[derive(Debug, clap::Args)]
struct ReactArgs {
    /// Path to a GitHub `issues` webhook payload (JSON).
    #[arg(long)]
    issue: PathBuf,
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Resolve and match only: print the agent, branch, and rendered prompt that
    /// would run, without cloning the repo, running the agent, or opening a PR.
    #[arg(long)]
    dry_run: bool,
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
struct StatusArgs {
    /// Optional single target label to report, e.g. `//third_party/backend:backend`.
    /// When omitted, every import/vendor/export target is reported.
    target: Option<String>,
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
}

#[derive(Debug, clap::Args)]
struct ReconcileArgs {
    /// Optional single target label to reconcile, e.g. `//third_party/backend:backend`.
    /// When omitted, every import/vendor/export target is reconciled.
    target: Option<String>,
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Bypass the agent-output cache: re-run every `agent_transform` (imports).
    #[arg(long)]
    refresh: bool,
    /// Push export branches but do not open GitHub PRs (print the `gh` command).
    #[arg(long)]
    no_pr: bool,
    /// Where `agent_transform`s run: `local` (harness on this machine) or
    /// `remote` (REAPI/BuildBuddy; configured via the `BUILDBUDDY_*` env vars).
    #[arg(long, value_enum, default_value_t)]
    executor: ExecutorArg,
}

/// CLI choice of where generative transforms execute.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
enum ExecutorArg {
    #[default]
    Local,
    Remote,
}

impl ExecutorArg {
    /// Resolve to a reconcile [`Executor`](capyfun::reconcile::Executor), reading
    /// `BUILDBUDDY_*` from the environment for the remote case.
    fn resolve(self) -> capyfun::reconcile::Executor {
        match self {
            ExecutorArg::Local => capyfun::reconcile::Executor::Local,
            ExecutorArg::Remote => {
                capyfun::reconcile::Executor::Remote(capyfun::reconcile::RemoteSettings::from_env())
            }
        }
    }
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
    /// Bypass the agent-output cache: re-run every `agent_transform` and
    /// re-materialize its patch (ignored by `vendor`).
    #[arg(long)]
    refresh: bool,
    /// Where `agent_transform`s run: `local` or `remote` (REAPI/BuildBuddy).
    /// Ignored by `vendor`.
    #[arg(long, value_enum, default_value_t)]
    executor: ExecutorArg,
}

#[derive(Debug, clap::Args)]
struct ExportArgs {
    /// Label of the `github_export` rule to run, e.g. `//sdk/go:public-go-sdk`.
    label: String,
    /// Monorepo root (the directory holding the root `SRC` file).
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Push the export branch but do not open a GitHub PR (print the `gh`
    /// command instead). Implied when the destination is a local repository.
    #[arg(long)]
    no_pr: bool,
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
        Command::Status(args) => run_status(args),
        Command::Reconcile(args) => run_reconcile(args),
        Command::Serve(args) => run_serve(args),
        Command::React(args) => run_react(args),
        Command::AgentRun(args) => run_agent_run(args),
    }
}

fn run_react(args: ReactArgs) -> Result<()> {
    use capyfun::forge::{Forge, GitHubAppForge, LocalForge};
    use capyfun::react::{self, ReactionIndex};

    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;

    let bytes = std::fs::read(&args.issue)
        .with_context(|| format!("reading issue payload {}", args.issue.display()))?;
    let payload: serde_json::Value =
        serde_json::from_slice(&bytes).context("parsing issue payload as JSON")?;
    let ev = react::parse_webhook_issue(&payload)
        .context("payload is not a recognizable GitHub `issues` webhook")?;

    let index = ReactionIndex::from_ir(&ir);
    let matched = react::match_issue(&index, &ev);
    if matched.is_empty() {
        println!(
            "no reaction matches {} issue #{} (action={}, labels={:?})",
            ev.repo, ev.number, ev.action, ev.labels
        );
        return Ok(());
    }

    if args.dry_run {
        for r in &matched {
            let (inv, prompt) =
                react::resolve_reaction_invocation(&ir, r, &ev, &args.root)?;
            println!("reaction {} would run:", r.label);
            println!("  repo:   {}", r.repo);
            println!("  branch: capyfun/issue-{} (base {})", ev.number, ev.default_branch);
            println!("  agent:  {} ({})", inv.agent_id, inv.model_id);
            println!("  prompt:");
            for line in prompt.lines() {
                println!("    {line}");
            }
        }
        return Ok(());
    }

    // Live: authenticate as a GitHub App when configured, else use local bare
    // repos (CAPYFUN_GITHUB_BASE) for a hermetic demo.
    let forge: Box<dyn Forge> = if std::env::var("CAPYFUN_GITHUB_APP_ID").is_ok() {
        Box::new(GitHubAppForge::from_env()?)
    } else {
        Box::new(LocalForge::new())
    };
    let runner = capyfun::engine::LiveRunner;

    let mut failed = 0usize;
    for r in &matched {
        match react::run_reaction(forge.as_ref(), &runner, &ir, r, &ev, &args.root) {
            Ok(outcome) => println!("{}", outcome.summary),
            Err(e) => {
                failed += 1;
                eprintln!("reaction {}: error: {e:#}", r.label);
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} reaction(s) failed", matched.len());
    }
    Ok(())
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
        &args.root,
        &args.addr,
        std::time::Duration::from_secs(args.interval_secs),
        args.once,
    )
}

fn run_status(args: StatusArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;

    let repo = git2::Repository::open(&args.root)
        .with_context(|| format!("opening monorepo at {}", args.root.display()))?;

    let statuses = capyfun::status::status_all(&repo, &ir, args.target.as_deref());
    if statuses.is_empty() {
        if let Some(t) = &args.target {
            bail!("no target labeled `{t}` (run `capyfun config` to list targets)");
        }
        println!("no import/vendor/export targets declared");
        return Ok(());
    }

    let mut behind = 0;
    for s in &statuses {
        println!("{}  {}  {}  {}", s.label, s.kind.as_str(), s.repo, s.summary());
        if s.state.is_actionable() {
            behind += 1;
        }
    }
    println!(
        "\n{} target(s); {} need reconcile",
        statuses.len(),
        behind
    );
    Ok(())
}

fn run_reconcile(args: ReconcileArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;

    let repo = git2::Repository::open(&args.root)
        .with_context(|| format!("opening monorepo at {}", args.root.display()))?;

    let target = args.target.as_deref();
    let mut matched = 0usize;
    let mut failed = 0usize;

    // The reconcile is level-triggered: each underlying action is idempotent, so
    // we simply run the action for every (filtered) target and report the diff it
    // applied. A failing target is reported but does not abort the others.
    let mut run = |label: &str, outcome: Result<String>| {
        matched += 1;
        match outcome {
            Ok(summary) => println!("{label}: {summary}"),
            Err(e) => {
                failed += 1;
                eprintln!("{label}: error: {e:#}");
            }
        }
    };

    use capyfun::reconcile::{do_export, do_import, do_vendor};
    let executor = args.executor.resolve();
    for i in &ir.imports {
        if target.is_none_or(|t| t == i.label) {
            run(&i.label, do_import(&repo, &ir, i, &args.root, args.refresh, &executor));
        }
    }
    for v in &ir.vendors {
        if target.is_none_or(|t| t == v.label) {
            run(&v.label, do_vendor(&repo, &ir, v));
        }
    }
    for e in &ir.exports {
        if target.is_none_or(|t| t == e.label) {
            run(&e.label, do_export(&repo, &ir, e, args.no_pr));
        }
    }

    if matched == 0 {
        if let Some(t) = target {
            bail!("no target labeled `{t}` (run `capyfun config` to list targets)");
        }
        println!("no import/vendor/export targets declared");
        return Ok(());
    }
    if failed > 0 {
        bail!("{failed} of {matched} target(s) failed to reconcile");
    }
    println!("\nreconciled {matched} target(s)");
    Ok(())
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

    let summary = capyfun::reconcile::do_vendor(&repo, &ir, vendor)?;
    println!("{}: {summary}", vendor.label);
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
                    "{}:{}  github_export repo={} branch={} from={} transforms={}",
                    e.package,
                    e.name,
                    e.repo,
                    e.branch,
                    from,
                    e.transforms.len()
                );
            }
            capyfun::config::Decl::GitRepo(g) => {
                let into = g.into.as_deref().unwrap_or("<package>");
                println!(
                    "{}:{}  git_repository repo={} commit={} into={}",
                    g.package, g.name, g.repo, g.commit, into
                );
            }
            capyfun::config::Decl::Harness(h) => {
                println!(
                    "{}:{}  harness kind={} plugins={} skills={}",
                    h.package,
                    h.name,
                    h.kind,
                    h.plugins.len(),
                    h.skills.len()
                );
            }
            capyfun::config::Decl::Model(m) => {
                let cred = m.credential.as_deref().unwrap_or("<default>");
                println!(
                    "{}:{}  model provider={} id={} credential={}",
                    m.package, m.name, m.provider, m.id, cred
                );
            }
            capyfun::config::Decl::Agent(a) => {
                println!(
                    "{}:{}  agent harness={} model={}",
                    a.package, a.name, a.harness, a.model
                );
            }
            capyfun::config::Decl::PromptTemplate(p) => {
                println!(
                    "{}:{}  prompt_template src={}",
                    p.package, p.name, p.src
                );
            }
            capyfun::config::Decl::OnIssue(r) => {
                println!(
                    "{}:{}  on_issue repo={} action={} label={} agent={}",
                    r.package,
                    r.name,
                    r.repo,
                    r.action.as_deref().unwrap_or("<any>"),
                    r.label.as_deref().unwrap_or("<any>"),
                    r.agent,
                );
            }
        }
    }
    Ok(())
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

    let summary = capyfun::reconcile::do_import(
        &repo,
        &ir,
        import,
        &args.root,
        args.refresh,
        &args.executor.resolve(),
    )?;
    println!("{}: {summary}", import.label);
    Ok(())
}

fn run_export(args: ExportArgs) -> Result<()> {
    let raw = capyfun::config::evaluate(&args.root)?;
    let ir = capyfun::ir::compile(&raw)
        .map_err(|diags| anyhow::anyhow!("config is invalid:\n  {}", diags.join("\n  ")))?;

    let export = ir
        .exports
        .iter()
        .find(|e| e.label == args.label)
        .ok_or_else(|| {
            let labels: Vec<&str> = ir.exports.iter().map(|e| e.label.as_str()).collect();
            anyhow::anyhow!(
                "no github_export labeled `{}` (available: {})",
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

    let summary = capyfun::reconcile::do_export(&repo, &ir, export, args.no_pr)?;
    println!("{}: {summary}", export.label);
    Ok(())
}

