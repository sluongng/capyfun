# CapyFun — hackathon judging guide

> **CapyFun makes LLM code transforms reproducible, reviewable, and replayable
> across Git repo boundaries.**

CapyFun is a Git repo synchronization engine that turns LLM-generated code changes
into deterministic, reviewable, content-addressed patches. It imports/exports code
across repo boundaries while safely applying agent transforms, verifying them, and
replaying them without repeated model cost.

## 60-second tour for a judge

```sh
cargo build --release
demo/full-loop.sh       # hermetic; no API key, no network, ~1 min including build
scripts/eval-agents.sh  # the measurable eval table (3 fixtures)
cargo test              # 220+ tests
```

`demo/full-loop.sh` runs the whole loop on local fixtures: an upstream API change
→ `capyfun import` → an agent transform (deterministic **fixture/mock** agent) →
a `go test` verifier → an export PR branch → a **replay** that hits the
content-addressed cache (model calls: 0, cost: $0.00).

---

## How the repo maps to the rubric

### Agentic leverage — *beyond a basic Claude Code / Cursor / Codex workflow*

The unit of work is not "an agent edits my repo." It is an **agent transform as a
typed, composable config edge between repos**, captured as a content-addressed
patch. Agents are declared in Starlark as `harness × model × prompt_template`
([`examples/transforms/`](../examples/transforms/)), attached to an import/export,
and run as part of a deterministic engine — so the same transform can be replayed,
diffed, bisected, and re-run through a different model.

- Closed, typed transform vocabulary (no arbitrary rewrite code):
  [`src/transform.rs`](../src/transform.rs),
  [`docs/design/transformations.md`](design/transformations.md).
- Pluggable execution backends behind one trait (`AgentRunner`): local CLI,
  remote REAPI/BuildBuddy, and the hermetic fixture/mock —
  [`src/engine/agent_exec.rs`](../src/engine/agent_exec.rs).
- The orchestration loop is *between* repos, level-triggered off forge events:
  [`docs/design/automation.md`](design/automation.md).

### Measurable impact — *benchmarks, evals, tests, savings*

- An eval harness with 3 fixtures and a results table:
  [`scripts/eval-agents.sh`](../scripts/eval-agents.sh),
  [`docs/evals.md`](evals.md).
- The headline number: **model runs once per unique input; every replay is free.**
  The demo prints first-run vs replay time and `cache miss → hit`.
- 220+ tests (`cargo test`), including the runner/cache/retry logic
  ([`src/engine/agent_exec/runner_tests.rs`](../src/engine/agent_exec/runner_tests.rs))
  and the engine's content-addressed cache ([`src/engine/tests.rs`](../src/engine/tests.rs)).

### Quality of the agent loop — *feedback, verification, iteration*

The loop is explicit and real:

```text
agent edits → verifier runs → if failed, feed stderr+diff back → retry once → materialize final patch
```

Implemented as [`VerifyingRunner`](../src/engine/agent_exec.rs): it runs the
verifier in the agent's workdir, appends the failure to the prompt on a miss, and
retries before aborting. The `dependency-modernize` eval demonstrates it
end-to-end — the agent's first attempt has a bug `go test` catches, and the
fed-back failure drives the fix on retry. The materialized patch is the
**verified** final state, so replays stay correct *and* free.

### Technical ambition — *real engineering, real constraints*

- JOSH-style Git tree projection + filter-repo-style history rewriting in Rust on
  `git2`, with a durable commit map and first-parent mirror semantics
  ([`src/engine.rs`](../src/engine.rs)).
- A content-addressed agent-output cache keyed on
  `(input subtree OID, rendered prompt, agent identity)` — credential excluded so
  key rotation doesn't invalidate output
  ([`src/engine.rs`](../src/engine.rs), `materialize_agent_patch`).
- A real REAPI v2 client (vendored protos, gRPC, BuildBuddy auth) to run agent
  transforms as remotely-cached Actions:
  [`src/remote/`](../src/remote/), [`docs/design/remote-execution.md`](design/remote-execution.md).
- A pure, statically-validated Starlark config compiled to a normalized IR before
  any Git mutation ([`src/config.rs`](../src/config.rs), [`src/ir.rs`](../src/ir.rs)).

### Cost–quality tradeoffs — *smart use of models, compute, parallelism*

> The expensive model only runs when the content-addressed input changes. Every
> replay is deterministic and free.

- Per-edge choice of `harness × model` lets you pick a cheap model for scrubs and
  a frontier model for migrations — all swappable as config.
- Three executors (`local` / `remote` / `fixture`) trade off cost, scale, and
  determinism behind one trait. Remote execution dedups identical work across a
  fleet via the Action Cache *for free* because output is content-addressed.
- Deterministic structural transforms (`replace`/`move`/…) cost **0 model calls**
  — the `oss-export-scrub` eval row shows this explicitly.
- Visible in output: import prints `cache Xh/Ym`; a no-op replay prints
  `already up to date (replayed tip from cache: N agent, cache 1h/0m)`.

### Collaboration — *human/agent division of work*

- Humans declare *edges and policy* (which repos, which transforms, which models)
  in reviewable Starlark; agents do the *mechanical migration* inside those
  guardrails. The transform vocabulary is closed, so config declares
  relationships, never arbitrary code.
- **Agents propose, CI/humans dispose:** export never pushes to a default branch;
  it opens a PR ([`src/reconcile.rs`](../src/reconcile.rs), `open_pr`). Every
  agent commit carries a `CapyFun-Agent` trailer and every mirror commit a
  `CapyFun-Origin` trailer, so every imported file is explainable back to its
  origin and the edge that produced it.
- This repo itself was built human + agent; see the memory of how the work was
  divided and the commit history.

### Demo clarity — *what, how, why*

- One command, hermetic, with a labeled summary block: [`demo/full-loop.sh`](../demo/full-loop.sh).
- A measurable table: [`scripts/eval-agents.sh`](../scripts/eval-agents.sh) → [`docs/evals.md`](evals.md).
- The README's 30-second pitch and 3-minute demo path: [`README.md`](../README.md).
- Design docs for every subsystem: [`docs/design/`](design/).

---

## What's implemented vs. planned

**Implemented today:** import round-trip (first-parent mirror + tip layer),
imperative + generative (agent) transforms executing with a content-addressed
cache, the **verify → retry agent loop**, the hermetic **fixture executor** + eval
harness, export (branch push + commit map + PR), vendoring, lockfile scaffolding
(`gen-go`/`gen-cargo`/`gen-npm`), a level-triggered reconciler, a GH-Archive
automation poller, and a REAPI/BuildBuddy remote executor for agent transforms.

**Planned:** broader triggers (issues, metric anomalies, production alerts),
richer source rules (import by commit/tag), and full fleet-scale orchestration
with quota/spend governance (see [`docs/design/`](design/) and the README's
*Runtime* section).
