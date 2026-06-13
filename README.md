# CapyFun

**The code import / export engine for monorepos.**

> The name: **`capyfun` == copy function** — it copies code across repo
> boundaries (with a capybara having fun along the way).

CapyFun moves code into and out of a monorepo while rewriting Git history so the
result looks native on both sides. It is the import/export subsystem of
[**tinytree**](https://tinytree.dev) — a forge built for monorepos.

> Shorthand: a Rust rewrite of Google's **Copybara** and Meta's **ShipIt**,
> built on **JOSH**-style Git object projection and **git-filter-repo**-style
> history rewriting — plus generative, agent-driven transforms that stay
> reproducible.

---

## Why this exists

Coding agents are trained on plain filesystem tools, so a monorepo is the
richest context interface they already know how to use. But good monorepo
tooling has historically only existed inside Google and Meta. tinytree changes
that with an infinitely-scaling, lazily-loaded, sparse-by-default tree — so you
can finally version-control the things you used to banish to LFS or
`.gitignore` (models, third-party deps), and agents can reach all of it.

CapyFun is the engine that populates that tree from the outside world and ships
selected paths back out.

### The orchestration loop is *between* repos

The interesting multi-agent loop is not inside one repo — it is *between* them.
You have a monorepo, I have one, the OSS world is a mesh; all of them emit and
react to events (push, release, PR-merged). Each node runs a level-triggered
reconciler — **events are hints, the commit map is the source of truth**.

- **Inner loop:** an agent works against code inside one repo.
- **Outer loop:** your export → an event → my import + transform → my export → …

Agents collaborate *stigmergically* — through the codebase, not a chat channel.

---

## What CapyFun does

CapyFun owns the two tree-boundary operations: **import** and **export**. They
are intentionally **asymmetric** — the two sides have different social
contracts, so they use different mechanisms. This is not a symmetric two-way
sync pipe.

### Import — replay upstream history faithfully

Take commits from an external Git repo and replay them into a path inside the
monorepo, rewriting each commit's tree so the imported source lives under its
destination prefix.

- Per-commit along the **first-parent** chain (history-preserving, **not**
  squashed). Merges are linearized to their first parent.
- Every mirrored commit is tagged with a `CapyFun-Origin: <sha>` trailer — the
  commit map, giving provenance back to upstream.
- Incremental and idempotent via a persisted commit map: re-running imports
  nothing new.

When an import declares extra work, it produces **two layers** so faithfulness
and local modification stay separable:

1. **Mirror layer** — the pristine first-parent reflection above.
2. **Tip layer** — declared patches and agent transforms applied on top as
   CapyFun-authored commits (`CapyFun-Patch` / `CapyFun-Agent` trailers),
   rebased onto the new tip on each incremental import.

### Export — publish changes as a GitHub Pull Request

Take changes to an exported monorepo path, rewrite them so the destination
prefix is stripped, push a branch, and open a **GitHub PR**. Export never pushes
to the destination's default branch directly — it respects the destination's
review process.

- Granularity: a reviewable changeset → one PR.
- Carries a `CapyFun-Export` commit-map trailer for incremental shipping.

---

## Transforms: agents as composable config

CapyFun has transforms, but they are a **closed, typed vocabulary** of builtins
— never arbitrary tree-rewrite code. The IR stays fully introspectable and
statically validatable.

The interesting transforms are **generative**: run a coding agent over the
incoming change (source + the change as context). Agents are composed in
**Starlark**, the language Bazel / Buck2 shops already use — pick a harness, a
model, and template the prompt, all as swappable code:

```python
harness(name = "claude_code", kind = "claude_code", plugins = ["//tools/plugins:bazel"])
model(name = "opus", provider = "anthropic", id = "claude-opus-4-8")
agent(name = "porter", harness = "//tools/harness:claude_code", model = "//tools/models:opus")

agent_transform(
    agent  = "//tools/agent:porter",
    prompt = template("//tools/agent/prompts:port"),
)
```

An agent transform's output is captured as a **content-addressed patch**, so:

- imports stay **reproducible** — re-import replays the recorded patch
  (`--refresh` regenerates);
- you can **replay your history through different agents/models** to benchmark
  cost, speed, and quality, and `git bisect` to find regressions.

**Agents propose, Bazel CI disposes** — transformed changes are verified
imperatively; nothing is trusted blindly.

### Example use cases

| Direction | Examples |
|-----------|----------|
| **Import** (outside → monorepo) | 🔍 scan every change for security/license issues · ⚙️ optimize an import to a goal ("tune these kernel changes for my datacenter's hardware profiles") · 🔁 migrate our app against upstream's new API |
| **Export** (monorepo → outside) | 📤 "update the OSS demo app **+ docs** to match my new SDK" before the PR goes out |

Same vocabulary, both directions.

---

## Quickstart

Requires a recent Rust toolchain (and `git`). `gh` is only needed to open real
PRs on export.

```sh
# build
cargo build --release

# evaluate the config and print the validated IR for an example
./target/release/capyfun check --root examples/transforms

# run the hermetic demo: builds a local upstream and runs a real import
# (no network — imports 2 commits + a tip patch into third_party/widget)
examples/transforms/materialize-widget.sh
```

What the demo shows:

- `imported 2 commit(s) · tip: 1 patch` — a real import, not a squash.
- `CapyFun-Origin: <sha>` on every commit — the commit map / provenance.
- `go.mod → toolchain go1.21.6` — a tip patch layered on the faithful mirror.

Hermetic smoke tests (build a local origin/destination in a temp dir, no
network):

```sh
scripts/smoke-import.sh
scripts/smoke-export.sh
```

---

## CLI

```
capyfun <command>
```

| Command | What it does |
|---------|--------------|
| `config` | Discover and evaluate `SRC` files; list the captured rules. |
| `check` | Evaluate → lower to IR → statically validate; print the IR as JSON. |
| `import <label>` | Replay an external repo's commits into a monorepo path (`--refresh` re-runs agent transforms). |
| `vendor <label>` | Vendor a pinned single-commit snapshot of a `git_repository` rule. |
| `export <label>` | Strip the prefix, push a branch, and open a GitHub PR (`--no-pr` to skip PR creation). |
| `gen-go` | Scaffold import `SRC` files from a `go.mod` / `go.sum`. |
| `gen-cargo` | Scaffold `git_repository` snapshot `SRC` files from `Cargo.toml` / `Cargo.lock`. |
| `gen-npm` | Scaffold a `git_repository` snapshot `SRC` from `package.json` / `package-lock.json`. |
| `serve` | Automation server: poll GH Archive and host the webhook endpoint (`--once` for a single cycle). |
| `agent-run` | Run a coding-agent harness over a prompt (proof of the `agent_transform` path). |

Labels are Bazel-style, e.g. `//third_party/backend:backend`.

---

## Config model

CapyFun uses its own narrow Starlark dialect with a two-file structure that
mirrors Bazel's `BUILD` / `.bzl` split:

- **`SRC` files** (literally named `SRC`) *instantiate* source rules. Each `SRC`
  file is a **package** at its directory.
- **`.scl` libraries** define reusable **macros** and constants, loaded with a
  `//`-anchored path. They never instantiate rules.

**Rules** (instantiated only in `SRC` files): `monorepo`, `github_import`,
`github_export`, `git_repository` (pinned snapshot), and the agent-tool rules
`harness`, `model`, `agent`, `prompt_template`.

**Value constructors** (pure, usable anywhere): `replace`, `move`, `copy`,
`apply_patch`, `rewrite_message`, `agent_transform`, and the `template` helper.

Both sets are a closed vocabulary — there is no custom `rule()` and no arbitrary
rewrite code. Config evaluation is **pure and deterministic** (no network, no
I/O); it compiles to a normalized IR that is statically validated before any Git
operation runs.

```python
# //SRC  (root: workspace anchor)
monorepo(name = "tinytree", default_branch = "main")

# //lib/github.scl  (library)
def vendored(name, repo, ref = "refs/heads/main", patches = []):
    github_import(name = name, repo = repo, ref = ref, patches = patches)

# //third_party/backend/SRC  (import lands under third_party/backend/)
load("//lib/github.scl", "vendored")
vendored(
    name = "backend",
    repo = "acme/backend",
    patches = ["patches/0001-pin-go-toolchain.patch"],
)
```

See [`examples/monorepo/`](examples/monorepo/) for a worked mini-monorepo,
[`examples/transforms/`](examples/transforms/) for the transform + agent
pipeline, and [`examples/export/`](examples/export/) for an export package.

---

## Architecture

```text
SRC files (per package) + .scl libraries (load())
  -> typed builtin calls (macros expanded)
  -> normalized CapyFun IR (labels resolved, paths package-anchored)
  -> static validation
  -> Git projection/rewrite engine (JOSH-like, filter-repo-like)
       - mirror: replay first-parent commits + structural transforms (per commit)
       - tip:    rebased local-modification transforms (patches, agent output)
  -> commit map (durable, content-addressed; incl. agent-output cache)
  -> import: commits replayed into the package's path
  -> export: branch pushed + GitHub PR opened
```

The **commit map** is the load-bearing data structure for both directions: it
records `origin_commit <-> monorepo_commit` so import is incremental and export
knows what has already shipped.

**Tech stack:** Rust · `git2` (libgit2) for Git plumbing · the `starlark` crate
for config · `gh` / `octocrab` for export PRs · coding-agent transforms shell
out to harness CLIs (Claude Code, Codex, …).

Design docs live in [`docs/`](docs/):
[transformations](docs/design/transformations.md),
[automation](docs/design/automation.md), and the
[import roadmap](docs/plans/import-roadmap.md).

---

## Runtime: from a laptop to a distributed agent farm

Everything above runs **locally today** — the automation poller consumes events
on one machine, and every agent transform is orchestrated against the local
checkout of the code. That is the right shape for a hackathon and for proving the
import/export round-trip, but it is not where this is headed.

The future runtime moves the same loop off the laptop:

- **Distributed event consumption.** Instead of a single-process poller, events
  (GH-Archive firehose, GitHub App webhooks, `ls-remote` backstop) land on a
  distributed server that fans them out to workers. The discipline is unchanged
  — **events are hints, the commit map is the source of truth** — so the server
  is level-triggered and crash-safe: it can drop or replay events without
  corrupting state.
- **A farm of compute sandboxes.** Agent transforms stop running against a local
  working tree and instead run in a farm of ephemeral, isolated sandboxes — each
  hydrated from the content-addressed tree it needs, each producing a
  content-addressed patch. Because agent output is already content-addressed,
  this maps cleanly onto **Bazel RBE + remote cache**: identical
  `(tree, agent, model, prompt)` work dedups for free, and a result computed once
  in the farm is reused everywhere.
- **Quota & spend governance.** Running agents at fleet scale needs guardrails.
  A quota system tracks **concurrency** (how many sandboxes a given import/export
  edge or tenant may run at once) and **spend** (token/compute budget per agent
  session), enforced before a sandbox is dispatched and metered as it runs. Each
  agent session is accountable back to the edge that requested it, so cost is
  attributable per import, per repo, per tenant.
- **Integration back into the forge.** The farm does not live in a vacuum: its
  outputs flow back into **tinytree's code review system and monorepo forge** —
  imports become reviewable changes, agent-authored tip layers and export PRs
  carry their provenance trailers into review, and the commit map ties every
  fleet-produced commit back to the origin and the edge that triggered it.

The invariants hold across the move: the engine stays deterministic, agent output
stays content-addressed and reproducible, and the commit map remains the
load-bearing record — the only thing that changes is *where* the work runs and
*how much* of it can run at once.

---

## Design invariants

- Import is per-commit along first-parent and history-preserving; it never
  squashes upstream history by default.
- Structural transforms rewrite every mirrored commit and keep the 1:1 origin
  mapping; local modifications ride in a separate rebased tip layer.
- Transforms are a closed, typed vocabulary, not arbitrary rewrite code.
- Export never pushes to a destination's default branch directly; it opens a PR.
- `github_import` preserves history; `git_repository` vendors a single pinned
  snapshot (one tree, no history, content-addressed by SHA).
- Imported files named `SRC` are renamed to `ORIG_SRC` so an upstream's own
  `SRC` files are not mistaken for CapyFun package markers.
- Exactly one `monorepo(...)` exists, in the root `SRC` file.
- An import's destination defaults to its `SRC` package's directory, so
  destinations cannot overlap by construction.
- Rewriting is deterministic; generative transforms are materialized into a
  content-addressed record so imports stay reproducible.
- Config is statically validated before any Git mutation. CapyFun operates on
  ordinary Git repositories at its edges.

---

## What CapyFun is *not*

- Not a general transformation DSL — transforms are a closed, typed vocabulary.
- Not a review product, CI system, or forge.
- Not a symmetric two-way merge engine — import and export are separate by
  design.
- Not a custom storage protocol — it works on ordinary Git repositories.

---

## Status

A hackathon project, biased toward small, runnable, tested milestones.

- **Built today:** import round-trip (mirror + tip layers), imperative and
  generative (agent) transforms executing, vendoring, lockfile scaffolding
  (`gen-go` / `gen-cargo` / `gen-npm`), export (branch push + commit map + PR),
  and a GH-Archive automation poller. 150+ tests.
- **Next:** the acting reconciler, scaling transforms onto Bazel RBE + remote
  cache (dedup is free because output is content-addressed), broader triggers
  (bug reports, metric anomalies, production alerts), and richer source rules
  (import by commit/tag, export straight to main).

Run `cargo test` and `cargo clippy` before finishing a milestone.

## Influences

[Copybara](https://github.com/google/copybara) ·
[ShipIt](https://github.com/facebook/fbshipit) ·
[JOSH](https://github.com/josh-project/josh) ·
[git-filter-repo](https://github.com/newren/git-filter-repo)

## License

Apache-2.0.
