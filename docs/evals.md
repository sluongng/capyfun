# CapyFun evals

CapyFun's pitch is that LLM code transforms can be made **reproducible,
reviewable, and replayable** across repo boundaries. This page is the measurable
evidence: a small, hermetic eval harness that runs agent transforms through the
real import/export engine and reports success, verification, runtime, and the
cache/cost tradeoff.

```sh
scripts/eval-agents.sh                 # run all fixtures, print the table
scripts/eval-agents.sh api-migration   # run a subset
```

Everything is hermetic: each fixture builds local Git repos in a temp directory
and runs the agent transform through the **`fixture` executor** — a deterministic
*mock* agent (recorded edits, **no model, no network, zero cost**). The mock runs
the exact same engine code path a real coding agent would (`materialize → diff →
content-addressed cache → replay`); only the in-workdir edit is recorded instead
of generated. The harness doubles as a test: a fixture whose verifier result does
not match its declared `EXPECT` makes the script exit non-zero.

## Fixtures

| Fixture | Direction | What it exercises |
|---------|-----------|-------------------|
| [`api-migration`](../tests/fixtures/evals/api-migration) | import + agent | Upstream renames `Connect()` → `New()`; an in-package caller is left un-migrated and no longer compiles. The agent migrates the call sites; `go test` verifies. Passes first try. |
| [`dependency-modernize`](../tests/fixtures/evals/dependency-modernize) | import + agent + **retry** | The updated test pins a new `Last[T]()` contract. The agent's first attempt has an off-by-one bug the verifier catches; the failure is fed back and the **retry** fixes it. Demonstrates the verify → retry loop. |
| [`oss-export-scrub`](../tests/fixtures/evals/oss-export-scrub) | export | Publish `sdk/go/client` out to a standalone repo, scrubbing `@--internal only--` lines and enabling `@--OSS only--` lines. Deterministic `replace` transform — **0 model calls** — verified to build clean with no internal markers leaked. |

## Results (hermetic, fixture/mock agents)

Regenerate with `scripts/eval-agents.sh`. Representative run (times vary by
machine; the Go build cache warms after the first compile):

| Fixture | Agent (harness + model) | Result | Verifier | Run 1 (ms) | Replay (ms) | Cache | Model calls | Est. cost |
|---------|-------------------------|--------|----------|-----------:|------------:|-------|------------:|-----------|
| api-migration | codex + gpt-5.5 (fixture/mock) | ✅ pass | `go test ./...` | 132 | 132 | miss → hit | 1 (mock) | $0.00 (mock) |
| dependency-modernize | claude_code + opus-4.8 (fixture/mock) | ✅ pass | `go test ./...` | 243 | 133 | miss → hit | 1 (mock) | $0.00 (mock) |
| oss-export-scrub | none (deterministic replace transform) | ✅ pass, scrubbed | `go build ./...` | 37 | — | n/a (no agent) | 0 | $0.00 |

`Model calls` is the cold-cache **miss** count — the number of model calls the
**real** `--executor local` path would make. Every replay is a content-addressed
cache hit and is free, which is the whole point: the expensive model runs once per
unique `(input subtree, prompt, agent identity)`; rerunning, bisecting, or
re-importing replays the recorded patch deterministically at no cost.

## Running the real agent path

The fixture executor is for hermetic, free CI/demo runs. To run an actual coding
agent, point the same edge at `--executor local` (a logged-in `claude` / `codex` /
`agy` CLI, or an API key — see [transformations.md](design/transformations.md),
*Credentials*) or `--executor remote` (REAPI/BuildBuddy — see
[remote-execution.md](design/remote-execution.md)):

```sh
capyfun import //third_party/greeter:greeter --root <monorepo> --executor local
```

A real run reports the same columns. The cost column is then a function of the
provider's token pricing and the patch size; it is **non-zero only on a cache
miss**. Rough order-of-magnitude for a small migration patch (a few files, one
agent call): cents per cold import on a frontier model, `$0.00` on every replay.
We deliberately do **not** print a fabricated dollar figure for the mock path —
mock rows are labeled `$0.00 (mock)` so the table never overstates what was
measured.

## Why mock, not live, by default

- **Determinism.** A fixture must produce the same patch every run so the eval is
  a real regression test, not a flake.
- **No secrets / no network.** Judges and CI can run it with one command.
- **Same code path.** The mock is injected through the `AgentRunner` trait the
  live and remote runners implement, so the materialize/diff/cache/replay loop
  under test is the production one — only the edit source differs.

See [`tests/fixtures/evals/README.md`](../tests/fixtures/evals/README.md) for the
fixture format and how to add one.
