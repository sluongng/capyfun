# CapyFun benchmarks: cost-effectiveness of agent transforms

A **benchmark** holds a code-movement task fixed and sweeps the agent cell —
`(harness × model × prompt)` — over it, replaying the same monorepo commits
through each combination and scoring every cell on three axes: **quality**,
**cost**, and **latency**. The output is a leaderboard and a Pareto frontier
that answers "which agent/model/harness/prompt is worth the money for *this*
kind of change?"

Read `../../CLAUDE.md` first for the thesis, then
[`transformations.md`](transformations.md) for how `agent_transform` runs and
[`../evals.md`](../evals.md) for the hermetic reproducibility harness this
extends.

> **Status:** Design. The load-bearing machinery already exists — first-parent
> replay (`src/engine.rs`: `first_parent_delta`, `replay_commit`), the
> `AgentRunner` trait and its `Live`/`Remote`/`Fixture`/`Verifying` runners
> (`src/engine/agent_exec.rs`), the content-addressed agent-output cache, and
> the eval table (`docs/evals.md`). A benchmark is a *matrix driver* over that
> machinery plus a scoring layer. Nothing here mutates the code-movement IR.

## Why this is different from `docs/evals.md`

`evals.md` proves one property — that a *single, fixed* agent transform is
**reproducible** (mock agent, deterministic patch, free replay). It answers
"does the pipeline work and stay cacheable?"

A benchmark answers a *comparative* question: hold the task fixed and the agent
**variable**, then rank cells by cost-effectiveness. The eval harness is the
hermetic floor; the benchmark is the live bake-off on top of it.

| | `evals.md` | benchmarks (this doc) |
|---|---|---|
| Agent | fixed (one cell) | swept (a matrix) |
| Executor | `fixture` (mock) | `local` / `remote` (real models) |
| Question | reproducible? | which is worth the cost? |
| Output | pass/fail table | leaderboard + Pareto frontier |
| Ground truth | declared `EXPECT` | verifier + the replayed real commit |

## Core idea: the replayed commit *is* the ground truth

The task fixes three things and the matrix varies the fourth:

1. **The input** — a set of upstream commits replayed onto a destination subtree
   via the existing mirror layer (`first_parent_delta` → `replay_commit`). Every
   cell sees byte-identical input trees.
2. **The verifier** — one command (`go test ./...`, `cargo test`, a build) and
   one retry budget, applied identically to every cell.
3. **The golden reference** — the *real* commit(s) that solved the task
   upstream. Because a benchmark **replays real history**, the human-authored
   fix is already in hand; it is the reference patch the agents are scored
   against. This is the key leverage of replaying real commits rather than
   synthetic fixtures.

The matrix varies the **agent cell**: which harness drives, which model thinks,
and which prompt is rendered.

```text
                       ┌─────────────────────────────────────┐
   fixed input commits │  mirror layer (first_parent_delta →  │
   (replayed once)     │  replay_commit): identical per cell  │
                       └───────────────────┬─────────────────┘
                                           │  same subtree OID + incoming diff
        ┌──────────────────────────────────┼──────────────────────────────────┐
        ▼                                   ▼                                   ▼
   cell (claude_code,opus,terse)    cell (codex,gpt-5.5,terse)    cell (pi,nemotron-3-ultra,detailed) …
        │                                   │                                   │
   Metered(Verifying(Runner))         Metered(Verifying(Runner))         Metered(Verifying(Runner))
        │  patch + metrics                  │                                   │
        ▼                                   ▼                                   ▼
   ┌──────────────────────────── score each cell ───────────────────────────────┐
   │ quality (verified? attempts? golden-distance? judge?)                        │
   │ cost    (model calls, tokens, $)        latency (wall ms)                    │
   └──────────────────────────────────┬──────────────────────────────────────────┘
                                       ▼
                       leaderboard + Pareto frontier + bench.json
```

## Declaring a benchmark

Benchmarks reuse the existing `harness` / `model` / `agent` / `prompt_template`
rules by **label** — they are never re-declared. They are written in a `BENCH`
file: a sibling of `SRC` that is discovered, evaluated with the same narrow
Starlark dialect, and may `load()` the same `.scl` libraries, but compiles to a
**separate Benchmark IR that never merges into the code-movement IR**. A `BENCH`
file declares no edges and mutates no Git; it only references existing labels.
(See *Invariants* for why this stays inside the "closed vocabulary" rule.)

```python
# //tools/models/SRC  (model rules referenced by benchmarks, declared once)
# Frontier, native-harness models:
model(name = "opus",      provider = "anthropic", id = "claude-opus-4-8")
model(name = "gpt",       provider = "openai",    id = "gpt-5.5")
model(name = "gemini",    provider = "google",    id = "gemini-3-pro")
# Frontier open-weight model served OpenAI-compatibly via the `pi` harness
# (Nebius; NEBIUS_API_KEY). The headline open-weight contender:
model(name = "nemotron",  provider = "nebius",    id = "nemotron-3-ultra",
      base_url = "https://api.studio.nebius.ai/v1")
# Cheap open-weight baseline, same route:
model(name = "qwen",      provider = "nebius",    id = "qwen-3-coder",
      base_url = "https://api.studio.nebius.ai/v1")

# //lib/bench.scl  (pure: a shared house list of top-of-the-line models)
def frontier_models():
    return [
        "//tools/models:opus",      # Claude Opus 4.8
        "//tools/models:gpt",       # GPT-5.5
        "//tools/models:gemini",    # Gemini 3 Pro
        "//tools/models:nemotron",  # Nemotron-3-Ultra (open weight, via Nebius)
    ]

# //third_party/greeter/BENCH
load("//lib/bench.scl", "frontier_models")

benchmark(
    name   = "greeter-migration",
    task   = "//third_party/greeter:greeter",   # import edge carrying the
                                                  # agent_transform + verifier
    golden = "refs/upstream/solved",             # the real solving commit(s):
                                                  # ground-truth reference patch
    reps   = 3,                                   # repeats per cell → variance

    # The matrix: cartesian product of these axes, minus `exclude`.
    matrix = matrix(
        harness = ["//tools/harness:claude_code", "//tools/harness:codex"],
        # frontier_models() = opus / gpt / gemini / nemotron; plus the cheap baseline
        model   = frontier_models() + ["//tools/models:qwen"],
        prompt  = ["//tools/prompts:terse", "//tools/prompts:detailed"],
        exclude = [
            # the native CLIs drive their own provider; route the open-weight
            # models (nemotron, qwen) through the `pi` HTTP harness only.
            cell(harness = "//tools/harness:claude_code",
                 model   = "//tools/models:nemotron"),
            cell(harness = "//tools/harness:codex",
                 model   = "//tools/models:nemotron"),
            cell(harness = "//tools/harness:claude_code",
                 model   = "//tools/models:qwen"),
            cell(harness = "//tools/harness:codex",
                 model   = "//tools/models:qwen"),
        ],
    ),

    scoring = scoring(
        # gate: an unverified cell scores quality 0 regardless of golden distance
        gate     = "verifier",
        quality  = {"verifier": 0.5, "golden_distance": 0.4, "judge": 0.1},
        weights  = {"quality": 0.6, "cost": 0.3, "latency": 0.1},
        judge    = "//tools/agent:judge",   # optional LLM-judge, see Scoring
    ),
)
```

`matrix(...)`, `cell(...)`, and `scoring(...)` are **pure value constructors**
(like `replace`/`template` in `transformations.md`) — usable in `.scl`
libraries, so a team can share `frontier_models()` or a house scoring profile.
`benchmark(...)` is the only rule, instantiated only from a `BENCH` file.

## Benchmark IR

Evaluation lowers the `BENCH` file to a flat, validated IR. Cells are the
cartesian product with `exclude` removed and labels resolved against the
code-movement IR (so a typo'd model label fails at validation, before any model
runs).

```rust
// src/bench/ir.rs  (sketch — mirrors src/ir.rs conventions)

pub struct Benchmark {
    pub label: String,
    pub task: String,            // label of an import edge with an agent_transform
    pub golden: Option<String>,  // git ref of the reference solution, if any
    pub reps: u32,
    pub cells: Vec<Cell>,        // already expanded + filtered
    pub scoring: Scoring,
}

pub struct Cell {
    pub harness: HarnessKind,    // resolved from //tools/harness:*
    pub provider: String,        // from the model rule
    pub model_id: String,
    pub credential: Option<String>,
    pub base_url: Option<String>,
    pub prompt_label: String,    // //tools/prompts:*
    pub prompt_text: String,     // template text, user-vars already substituted
}

impl Cell {
    /// One AgentInvocation per cell — the exact struct the engine already runs.
    /// `agent_id` encodes the cell so the CapyFun-Agent trailer and the cache
    /// key disambiguate cells that share an input but differ in harness/model.
    fn invocation(&self) -> AgentInvocation {
        AgentInvocation {
            harness: self.harness,
            provider: self.provider.clone(),
            model_id: self.model_id.clone(),
            credential: self.credential.clone(),
            base_url: self.base_url.clone(),
            prompt: self.prompt_text.clone(),
            agent_id: format!("bench:{}/{}", self.label_key(), /*rep*/),
            paths: vec![],
        }
    }
    fn label_key(&self) -> String {
        format!("{}+{}+{}", self.harness.as_str(), self.model_id, self.prompt_label)
    }
}

pub struct Scoring {
    pub gate: Gate,                       // Verifier | None
    pub quality: Vec<(QualityAxis, f64)>, // axis -> weight, sums to 1
    pub weights: CompositeWeights,        // quality / cost / latency, sums to 1
    pub judge: Option<String>,            // judge agent label
}
```

## Running a cell: compose, don't fork

A cell is **the existing tip-layer agent run with the cell's invocation
substituted**, wrapped in two decorators over the `AgentRunner` trait. The
benchmark introduces exactly one new runner — `MeteredRunner` — and reuses
everything else.

`AgentRunner` today (`src/engine/agent_exec.rs`):

```rust
pub trait AgentRunner {
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()>;
}
```

`VerifyingRunner` already wraps any runner with the verify → retry loop, and the
content-addressed cache already keys on `(input subtree OID, incoming change,
rendered prompt, agent identity)` — so distinct cells never collide and re-runs
of the *same* cell are free. We add a metering decorator:

```rust
// src/bench/metered.rs

/// Wraps any AgentRunner to capture per-run cost & latency without touching the
/// materialize → diff → cache → commit loop. Mirrors the VerifyingRunner
/// decorator pattern (src/engine/agent_exec.rs).
pub struct MeteredRunner<'a> {
    inner: &'a dyn AgentRunner,
    sink:  &'a Meter,   // interior-mutable accumulator (RefCell/atomic)
}

impl AgentRunner for MeteredRunner<'_> {
    fn run(&self, inv: &AgentInvocation, prompt: &str, workdir: &Path) -> Result<()> {
        let started = self.sink.clock();          // monotonic; injected, see note
        let r = self.inner.run(inv, prompt, workdir);
        let usage = self.sink.collect_usage(inv, workdir);  // see "Cost accounting"
        self.sink.record(RunSample {
            model_call: true,                     // this call was a cold miss
            wall_ms: self.sink.clock() - started,
            tokens_in: usage.tokens_in,
            tokens_out: usage.tokens_out,
            usd: price_of(&inv.provider, &inv.model_id, &usage),
        });
        r
    }
}
```

The stack for a live cell is therefore:

```rust
let live     = LiveRunner;                                  // or RemoteRunner
let metered  = MeteredRunner { inner: &live, sink: &meter };
let verifying = VerifyingRunner::new(&metered, &verify_cmd, retries);
run_import_engine(/* same call do_import makes */, &verifying)?;
```

Because `MeteredRunner` sits *inside* `VerifyingRunner`, each retry is metered
separately, so "attempts-to-green" and the *total* token spend of a flaky cell
are both captured. On a cache hit the inner runner is never invoked, so a
replayed cell records `model_call: false` and `usd: 0` — the free-replay
property survives the benchmark unchanged.

> **Note on `clock()`:** scripts/engine code may not call wall-clock freely in
> deterministic contexts. The meter takes a clock handle injected by the CLI
> driver (real monotonic clock for live runs), so the engine core stays pure.

## The driver loop

```rust
// src/bench/run.rs  (sketch)

pub fn run_benchmark(repo: &Repository, ir: &Ir, bench: &Benchmark,
                     root: &Path, exec: &Executor) -> Result<Report> {

    // 1. Resolve the task edge and replay its input ONCE. Every cell shares
    //    this mirror state; the tip layer (the agent) is what varies.
    let task   = ir.find_import(&bench.task)?;
    let input  = prepare_mirror(repo, task, root)?;   // first_parent_delta +
                                                       // replay_commit, as do_import does
    let golden = bench.golden.as_deref()
        .map(|r| reference_patch(repo, &input, r)).transpose()?;

    let mut rows = Vec::new();
    for cell in &bench.cells {
        for rep in 0..bench.reps {
            let inv   = cell.invocation_for_rep(rep);  // rep folded into agent_id
                                                        // => distinct cache key
            let meter = Meter::new(real_clock());

            // Run the tip layer for this cell over the shared input subtree.
            // Returns the materialized patch (the agent's edit) + verifier outcome.
            let outcome = run_tip_cell(repo, &input, &inv, task.verify(), exec, &meter)?;

            rows.push(Sample {
                cell:    cell.label_key(),
                rep,
                quality: score_quality(&outcome, golden.as_ref(), &bench.scoring)?,
                cost:    meter.totals(),       // model_calls, tokens, usd
                latency: meter.wall_ms(),
                patch:   outcome.patch_oid,    // content-addressed; auditable
            });
        }
    }
    Ok(Report::aggregate(bench, rows))   // mean/median/stddev per cell + ranking
}
```

`run_tip_cell` is a thin wrapper that builds the `Metered(Verifying(inner))`
stack above and calls the same engine entrypoint `do_import` uses for the tip
layer — so a benchmarked agent runs the **production code path**, not a parallel
one. The only difference from `do_import` is that the result branch is discarded
(benchmarks never advance a real ref) and the metrics are kept.

## Scoring

### Quality axes

Each is normalized to `0.0..=1.0`; `gate = "verifier"` means a cell that never
goes green scores quality **0** no matter how close its patch looks.

```rust
fn score_quality(o: &CellOutcome, golden: Option<&Patch>, s: &Scoring) -> Result<f64> {
    if matches!(s.gate, Gate::Verifier) && !o.verified {
        return Ok(0.0);                       // hard gate
    }
    let mut q = 0.0;
    for (axis, w) in &s.quality {
        q += w * match axis {
            // 1.0 if verifier green within budget, else 0.
            QualityAxis::Verifier => if o.verified { 1.0 } else { 0.0 },

            // fewer retries to green is better: 1/attempts (1 try => 1.0).
            QualityAxis::Attempts => 1.0 / (o.attempts as f64).max(1.0),

            // similarity of the agent's patch to the real solving commit.
            QualityAxis::GoldenDistance => match golden {
                Some(g) => patch_similarity(&o.patch, g),  // see below
                None    => 0.0,                            // no ground truth → no credit
            },

            // optional LLM-judge rubric score (correctness/minimality/style).
            QualityAxis::Judge => o.judge_score.unwrap_or(0.0),
        };
    }
    Ok(q)
}
```

**`patch_similarity`** compares the agent's materialized patch against the
golden patch. Cheap-to-rich ladder (start at the top, add lower rungs later):

1. **File-set F1** — precision/recall over the set of touched files. Catches
   "edited the wrong files" instantly, no language tooling.
2. **Normalized line-diff ratio** — `1 - levenshtein(norm(a), norm(b)) / len`,
   where `norm` strips whitespace/comment churn. Language-agnostic.
3. **AST-aware distance** (later, per-language) — robust to formatting and
   reordering; gated behind a tree-sitter parser per ecosystem.

Golden distance is a *similarity*, not a correctness oracle — a different but
correct fix can score low. That is why the **verifier is the primary gate** and
golden distance is a weighted signal, not a pass/fail.

### LLM-judge (optional, recorded not trusted blindly)

When `scoring.judge` is set, a judge agent scores each verified patch on a
rubric. It is nondeterministic, so:

- the judge's own `(harness, model, prompt)` identity is recorded in the report;
- the judge runs through the **same content-addressed cache** keyed on
  `(patch, rubric, judge identity)`, so re-scoring a run is free and stable;
- judge weight should stay small (a tiebreaker), and the report always shows the
  verifier/golden columns separately so a judge can never silently dominate.

```python
# //tools/prompts:judge-rubric  (referenced by the judge agent)
# Score 0..1 on: (a) does the patch address the task? (b) is it minimal?
# (c) idiomatic for the surrounding code? Output strict JSON {"score": <0..1>}.
```

### Composite & cost-effectiveness

Per cell we aggregate across reps (median for robustness, plus stddev so a
high-variance model is visible), then compute two views:

```rust
fn cost_effectiveness(agg: &CellAgg, w: &CompositeWeights) -> Ranked {
    // Normalize cost & latency to 0..1 *within this benchmark run* (min-max
    // across cells) so the scalar weighting is comparable, not absolute-$ noise.
    let cost_score = 1.0 - norm(agg.usd_median,  run.usd_range);     // cheaper → higher
    let lat_score  = 1.0 - norm(agg.wall_median, run.wall_range);    // faster  → higher

    // (1) Scalar utility — one number for a leaderboard.
    let utility = w.quality * agg.quality_median
                + w.cost    * cost_score
                + w.latency * lat_score;

    // (2) Raw ratio — "quality per dollar", the headline cost-effectiveness #.
    let value_per_usd = agg.quality_median / (agg.usd_median + 1e-6);

    Ranked { utility, value_per_usd, pareto: false /* filled below */ }
}
```

Because any single weighting is arguable, the report **always** computes the
**Pareto frontier** over `(quality↑, usd↓, latency↓)` and flags the
non-dominated cells. The scalar `utility` ranks within that; the frontier is the
honest answer.

```rust
// A cell is on the frontier if no other cell beats it on every axis at once.
fn mark_pareto(cells: &mut [CellAgg]) {
    for i in 0..cells.len() {
        cells[i].pareto = !cells.iter().enumerate().any(|(j, o)| j != i
            && o.quality_median >= cells[i].quality_median
            && o.usd_median     <= cells[i].usd_median
            && o.wall_median    <= cells[i].wall_median
            && (o.quality_median > cells[i].quality_median
                || o.usd_median  < cells[i].usd_median
                || o.wall_median < cells[i].wall_median));
    }
}
```

## Cost accounting

`MeteredRunner::collect_usage` resolves token counts per harness, in order of
fidelity:

- **`pi` (HTTP harness):** usage is in the OpenAI-compatible response body —
  exact `prompt_tokens` / `completion_tokens`.
- **`claude_code` / `codex` CLIs:** parse the harness's own usage report
  (session log / `--json` summary) emitted to the captured stderr/stdout.
- **`remote` (REAPI):** the Action's auxiliary metadata carries usage when the
  remote wrapper records it; otherwise fall back to the harness log inside the
  output tree.
- **Fallback / fixture:** if no usage is reported, estimate from prompt + patch
  byte length via a tokenizer-ratio heuristic and **mark the row `~est`** — never
  silently present an estimate as measured (same discipline as `evals.md`'s
  `$0.00 (mock)` labeling).

Dollars come from a small, checked-in, dated price table — *not* hardcoded in
logic:

```python
# //tools/prices.scl  (data, version-controlled, dated)
PRICES = {
    # provider/model : ($/1M input, $/1M output), as of 2026-06-01
    "anthropic/claude-opus-4-8": (15.0, 75.0),
    "openai/gpt-5.5":            ( 5.0, 40.0),
    "google/gemini-3-pro":       ( 4.0, 30.0),
    "nebius/nemotron-3-ultra":   ( 0.9,  2.6),   # open weight, frontier-class
    "nebius/qwen-3-coder":       ( 0.4,  1.6),   # cheap baseline
}
```

```rust
fn price_of(provider: &str, model: &str, u: &Usage) -> f64 {
    let (pin, pout) = PRICES.get(&format!("{provider}/{model}")).copied()
        .unwrap_or((0.0, 0.0));   // unknown model → $0 + a loud warning in report
    (u.tokens_in as f64 / 1e6) * pin + (u.tokens_out as f64 / 1e6) * pout
}
```

The price table is dated and committed so a benchmark from six months ago can be
re-priced or re-read against the prices that were true when it ran.

## CLI

```sh
# Run a benchmark; default executor is local (real models, real money).
capyfun bench //third_party/greeter:greeter-migration --root <monorepo>

capyfun bench //…:greeter-migration \
    --root <monorepo> \
    --reps 5 \
    --executor remote \              # REAPI/BuildBuddy fan-out (see remote-execution.md)
    --filter 'model=opus,gpt-5.5' \  # subset the matrix without editing BENCH
    --out bench.json                 # machine-readable record for regression tracking

# Compare a new run to a stored baseline: flags regressions & wins per cell.
capyfun bench //…:greeter-migration --root <monorepo> --baseline bench.json
```

`--executor remote` is the natural fit: cells are independent, so the matrix
fans out across REAPI workers and the whole bake-off finishes in roughly the
wall time of its slowest single cell. Each cell's output is still
content-addressed, so a re-run only pays for cache misses.

## Report

Human table (extends the `evals.md` columns with the matrix + ranking):

```text
benchmark: greeter-migration   task: //third_party/greeter:greeter   reps: 3
golden: refs/upstream/solved   verifier: go test ./...   executor: local

 cell (harness+model+prompt)         ver-  attempts  golden  quality   $/run   wall   value/$   pareto
                                     ified  (median)  dist            (median) (ms)
 ──────────────────────────────────────────────────────────────────────────────────────────────────
 claude_code+opus-4.8+detailed        3/3      1.0    0.94    0.97     0.043   11.2s    22.6       ★
 codex+gpt-5.5+terse                  3/3      1.0    0.82    0.91     0.011    8.4s    82.7       ★
 pi+nemotron-3-ultra+detailed         3/3      1.3    0.74    0.84     0.004    7.0s   210.0       ★
 pi+qwen-3-coder+detailed             2/3      1.7    0.61    0.58     0.002    6.1s   290.0       ★
 claude_code+opus-4.8+terse           3/3      1.3    0.88    0.90     0.038    9.9s    23.7
 codex+gpt-5.5+detailed               3/3      1.0    0.79    0.89     0.014    9.1s    63.6
 pi+gemini-3-pro+detailed             3/3      1.0    0.85    0.92     0.030    8.7s    30.7
 pi+nemotron-3-ultra+terse            2/3      1.5    0.66    0.61     0.003    6.4s   203.3
 pi+qwen-3-coder+terse                1/3      2.0    0.40    0.31 ~est 0.002    5.8s   155.0
 ──────────────────────────────────────────────────────────────────────────────────────────────────
 ★ = Pareto-optimal (no cell beats it on quality, cost, and latency at once).
 Four honest winners: best-quality (opus-4.8), best-value-that-clears-the-bar
 (gpt-5.5), best open-weight (nemotron-3-ultra — frontier-class quality at a
 fraction of the cost), and cheapest-that-still-passes (qwen-3-coder).
```

`bench.json` carries the full record — every cell's identity tuple (harness
kind, model id, prompt hash, and, when available, plugin/skill digests),
per-rep raw samples, the dated price table used, and the patch OIDs — so a run
is reproducible and auditable, and `--baseline` can diff two runs cell-by-cell.

## Invariants

- A benchmark **never advances a real ref and never opens a PR.** It replays
  into throwaway state, scores, and discards. It is pure measurement.
- A `BENCH` file **declares no code-movement edges** and compiles to a separate
  Benchmark IR that never merges into the code-movement IR. It only *references*
  existing `harness`/`model`/`agent`/`prompt_template`/import labels. This keeps
  the code-movement IR a closed vocabulary; the benchmark layer is eval-only
  tooling sitting beside it, the same way `evals.md` sits beside the engine.
- Every cell runs the **production engine path** (`Metered(Verifying(Live|Remote))`
  over the real tip layer), never a benchmark-only reimplementation, so a
  benchmark measures what an import would actually do.
- **Input is replayed once and shared** byte-for-byte across all cells; only the
  agent cell varies. Fairness is structural, not promised.
- Cost is **measured, not fabricated**; unmeasurable token counts are estimated
  and visibly marked `~est`, and dollars come from a dated, committed price
  table.
- Results are **reproducible**: same cells + same cache → same patches, replayed
  free. Model nondeterminism is captured as cross-rep variance (stddev), not
  hidden.
- The honest answer is the **Pareto frontier**; the scalar leaderboard is a
  convenience layered on top, never a substitute.

## Milestones

- **K1 — Matrix over fixtures.** Reuse the three `evals.md` fixtures; sweep two
  mock cells each; emit the leaderboard table with cost columns = mock/$0. Pure
  plumbing, hermetic, no new money. Proves the driver + report.
- **K2 — `MeteredRunner` + price table.** Add the metering decorator and dated
  price table; capture tokens/$ from the `pi` HTTP harness first (exact usage).
- **K3 — Golden distance.** File-set F1 + normalized line-diff against a real
  replayed commit on one live import. First true cost-vs-quality table.
- **K4 — Pareto + variance.** `reps > 1`, stddev, frontier marking, scalar
  utility, and the `★` report.
- **K5 — `remote` fan-out + `--baseline`.** Matrix across REAPI workers;
  regression diffing against a stored `bench.json`.
- **K6 — LLM-judge axis.** Optional rubric judge, cached and identity-recorded,
  small weight.

## Open questions

- **Variance budget.** How many reps are enough to separate two close cells
  without burning money? Adaptive stopping (run more reps only where confidence
  intervals overlap) vs. a flat `reps`.
- **Golden distance for divergent-but-correct fixes.** Should a verifier-green
  patch that is far from golden be *rewarded* for finding a better solution, or
  is golden distance only ever a tiebreak? Leaning: tiebreak only; verifier is
  truth.
- **Token accounting for CLI harnesses.** Parsing `claude_code`/`codex` usage
  logs is brittle across versions. Is a wrapper that forces a `--json` usage
  summary worth standardizing, or do we lean on `remote` metadata?
- **Cross-task aggregation.** A single benchmark ranks cells for *one* kind of
  change. How do we roll several benchmarks (migration, dep-bump, scrub) into a
  per-model scorecard without implying a false total order?
- **Prompt-vs-model confounding.** A cheap model with a great prompt can beat an
  expensive one with a poor prompt. Do we report marginal effects per axis
  (hold two fixed, vary one) to disentangle, or leave that to the frontier?
</content>
</invoke>
