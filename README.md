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

**Agents propose, the verifier disposes** — nothing is trusted blindly. An agent
transform runs inside an explicit verify → retry loop:

```text
agent edits → verifier runs → if failed, feed stderr+diff back → retry once → materialize final patch
```

The verifier is any command (`go test ./...`, `cargo test`, a fixture check). On
failure its output is appended to the prompt and the agent retries; only the
**verified** final state is materialized to the cache. So replays are correct
*and* free. (See [`VerifyingRunner`](src/engine/agent_exec.rs).)

Where an agent runs is a swappable backend behind one trait — pick per run with
`--executor`:

| Executor | Runs the harness… | Use for |
|----------|-------------------|---------|
| `local` (default) | on this machine (logged-in `claude`/`codex`/`agy`, or an API key) | real transforms |
| `remote` | as a REAPI Action on BuildBuddy, Action-Cache–served | fleet scale, shared cache |
| `fixture` | as a deterministic recorded mock (no model, no network) | hermetic demos, evals, CI |

Because agent output is content-addressed, identical `(input, agent, model,
prompt)` work dedups for free — locally or across a remote fleet.

### Example use cases

| Direction | Examples |
|-----------|----------|
| **Import** (outside → monorepo) | 🔍 scan every change for security/license issues · ⚙️ optimize an import to a goal ("tune these kernel changes for my datacenter's hardware profiles") · 🔁 migrate our app against upstream's new API |
| **Export** (monorepo → outside) | 📤 "update the OSS demo app **+ docs** to match my new SDK" before the PR goes out |

Same vocabulary, both directions.

---

## Reactions: react to forge events, open a PR

Beyond *moving* code across tree boundaries, CapyFun **reacts** to GitHub events
by running a coding agent and opening a PR — the generative counterpart to
import/export. The first reaction is **`on_issue`**: an issue labeled for an
agent gets a prototype PR.

```python
# //automation/SRC  — issue → agent → PR
on_issue(
    name   = "prototype-assigned",
    repo   = "acme/backend",            # owner/name the GitHub App is installed on
    action = "labeled",
    label  = "assign-agent",
    agent  = "//tools/agent:reviewer",  # reuses the same agent/harness/model rules
    prompt = template("//tools/agent/prompts:prototype-issue"),
)
```

For a matched issue: clone → branch `capyfun/issue-<n>` → run the agent (with
`{{issue_title}}`/`{{issue_body}}`/… context vars) → commit with `CapyFun-Agent` /
`CapyFun-Issue` trailers → push → open a PR (`Closes #<n>`). **No edits → no PR.**

- **GitHub App identity** — mints an RS256 App JWT, exchanges it for an
  installation token, and uses it for clone/push and the PR call. App credentials
  come from the environment, never the repo. A `LocalForge` runs the whole loop
  hermetically against local bare repos.
- **Webhook hardening** — the endpoint **fails closed** without a secret, verifies
  `X-Hub-Signature-256` (HMAC-SHA256, constant-time) before acting, routes by
  `X-GitHub-Event`, and dedupes redeliveries by `X-GitHub-Delivery`.

This is a **deliberate scope change**: CapyFun now acts on forge events and opens
PRs, but it still **never merges** — a human (or policy) reviews. Try it
hermetically with `scripts/smoke-react.sh`; see
[`docs/design/reactions.md`](docs/design/reactions.md) and
[`examples/reactions/`](examples/reactions/).

---

## 30-second pitch

> **CapyFun makes LLM code transforms reproducible, reviewable, and replayable
> across Git repo boundaries.** It imports/exports code between repos, applies
> agent transforms inside a closed typed vocabulary, **verifies** them, and
> materializes each one to a **content-addressed patch** — so the expensive model
> runs only when the input changes and every replay is deterministic and free.

Compared with running Claude Code / Cursor / Codex ad-hoc on a repo: those produce
a one-off diff with no provenance, no cache, and no replay. CapyFun makes the
transform a typed config edge with a `CapyFun-Agent` trailer, a verifier, and a
cache key — so it can be reviewed, re-run through a different model, bisected, and
re-imported at zero cost. Compared with Copybara/JOSH/git-filter-repo: same
origin/destination/commit-map discipline, but a Rust core with generative
transforms that stay reproducible.

## Quickstart — the full loop in under 3 minutes

Requires a recent Rust toolchain, `git`, and `go` (the demo's verifier). No API
key and no network needed — the demo's agent runs as a deterministic **mock**.

```sh
cargo build --release

# The whole loop, hermetic: upstream change → import → agent transform →
# verifier → export PR branch → free cache-hit replay.
demo/full-loop.sh

# The measurable eval table across 3 fixtures (doubles as a test).
scripts/eval-agents.sh
```

`demo/full-loop.sh` prints a labeled summary:

```text
Imported:            2 upstream commits (first-parent mirror, history preserved)
Agent:               codex + gpt-5.5 (fixture/mock)
Patch cache:         miss first run (1 agent call), hit second run (1 cache hit)
Verifier:            pass   (go test ./..., with one verify→retry loop available)
Export:              branch 'capyfun/export-go-sdk' produced (internal-only scrubbed)
First run time:      141 ms   (import + agent + verifier)
Replay time:         27 ms    (deterministic, cache-served)
Model calls (real):  0   — the fixture/mock agent makes no model calls
Estimated model cost: $0.00 (mock).  The expensive model only runs on a
                     content-addressed cache MISS; every replay is free.
```

Other hermetic entry points (no network):

```sh
./target/release/capyfun check --root examples/transforms  # config → validated IR
examples/transforms/materialize-widget.sh                  # a real import (mirror + tip patch)
scripts/smoke-import.sh ; scripts/smoke-export.sh          # import/export round-trip in a temp dir
```

## Measurable results

See [`docs/evals.md`](docs/evals.md). The eval harness runs three fixtures through
the real engine with the hermetic mock agent and reports a table:

| Fixture | Result | Verifier | Cache | Model calls | Est. cost |
|---------|--------|----------|-------|------------:|-----------|
| api-migration | ✅ pass | `go test ./...` | miss → hit | 1 (mock) | $0.00 |
| dependency-modernize | ✅ pass (via verify→retry) | `go test ./...` | miss → hit | 1 (mock) | $0.00 |
| oss-export-scrub | ✅ pass, scrubbed | `go build ./...` | n/a (no agent) | 0 | $0.00 |

The key result: **model calls happen once per unique `(input subtree, prompt,
agent identity)`; every replay is a content-addressed cache hit and is free.**
220+ `cargo test` tests cover the runner/cache/retry logic and the engine.

---

## CLI

```
capyfun <command>
```

| Command | What it does |
|---------|--------------|
| `config` | Discover and evaluate `SRC` files; list the captured rules. |
| `check` | Evaluate → lower to IR → statically validate; print the IR as JSON. |
| `import <label>` | Replay an external repo's commits into a monorepo path (`--refresh` re-runs agent transforms; `--executor local\|remote\|fixture` selects where they run). |
| `vendor <label>` | Vendor a pinned single-commit snapshot of a `git_repository` rule. |
| `export <label>` | Strip the prefix, push a branch, and open a GitHub PR (`--no-pr` to skip PR creation). |
| `gen-go` | Scaffold import `SRC` files from a `go.mod` / `go.sum`. |
| `gen-cargo` | Scaffold `git_repository` snapshot `SRC` files from `Cargo.toml` / `Cargo.lock`. |
| `gen-npm` | Scaffold a `git_repository` snapshot `SRC` from `package.json` / `package-lock.json`. |
| `serve` | Automation server: poll GH Archive and host the (HMAC-verified) webhook endpoint (`--once` for a single cycle). |
| `react --issue <payload>` | Run an `on_issue` reaction from a GitHub `issues` webhook payload: clone → agent → PR (`--dry-run` to preview). |
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
`github_export`, `git_repository` (pinned snapshot), the agent-tool rules
`harness`, `model`, `agent`, `prompt_template`, and the reaction rule `on_issue`
(run an agent on a labeled issue and open a PR).

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
[automation](docs/design/automation.md),
[reactions](docs/design/reactions.md),
[remote execution](docs/design/remote-execution.md), and the
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
- Not a review product or CI system. CapyFun now *reacts* to forge events
  (issue → agent → PR) and opens PRs, but it never merges — a human reviews.
- Not a symmetric two-way merge engine — import and export are separate by
  design.
- Not a custom storage protocol — it works on ordinary Git repositories.

---

## Status

A hackathon project, biased toward small, runnable, tested milestones.

- **Built today:** import round-trip (mirror + tip layers), imperative and
  generative (agent) transforms executing with a content-addressed cache, the
  **verify → retry agent loop**, three executors (local / remote-REAPI /
  hermetic fixture), an **eval harness** with measurable results
  ([`docs/evals.md`](docs/evals.md)), a level-triggered reconciler, vendoring,
  lockfile scaffolding (`gen-go` / `gen-cargo` / `gen-npm`), export (branch push
  + commit map + PR), a GH-Archive automation poller, a REAPI/BuildBuddy remote
  executor for agent transforms, and **`on_issue` reactions** (issue → agent → PR
  via a GitHub App, with HMAC-verified webhooks). 220+ tests.
- **Next:** more reaction triggers (`on_push` / `on_tag`, then bug reports,
  metric anomalies, production alerts), an agent sandbox, richer source rules
  (import by commit/tag, export straight to main), and fleet-scale orchestration
  with quota/spend governance.

For judges: [`docs/hackathon-judging.md`](docs/hackathon-judging.md) maps the repo
to the rubric.

Run `cargo test` and `cargo clippy` before finishing a milestone.

## Influences

[Copybara](https://github.com/google/copybara) ·
[ShipIt](https://github.com/facebook/fbshipit) ·
[JOSH](https://github.com/josh-project/josh) ·
[git-filter-repo](https://github.com/newren/git-filter-repo)

## License

Apache-2.0.
