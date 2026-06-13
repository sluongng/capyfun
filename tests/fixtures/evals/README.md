# Eval fixtures

Hermetic, deterministic fixtures for the agent-transform eval harness
([`scripts/eval-agents.sh`](../../../scripts/eval-agents.sh)) and the full-loop
demo ([`demo/full-loop.sh`](../../../demo/full-loop.sh)). See
[`docs/evals.md`](../../../docs/evals.md) for results and rationale.

Each fixture runs through the real `capyfun` engine with the **`fixture`
executor** — a deterministic *mock* agent ([`FixtureRunner`]) that runs a recorded
shell script instead of a model. No network, no model, zero cost.

## Layout

```text
<fixture>/
├── meta.env            # scenario parameters (sourced; quote values with spaces)
├── monorepo/           # the monorepo tree (root SRC, tools/, the import/export edge)
├── commits/01, 02, …   # (import fixtures) one upstream commit per dir, applied in order
└── agent/<name>.sh     # (import fixtures) the recorded mock agent, keyed by agent label
```

`meta.env` fields:

| Field | Meaning |
|-------|---------|
| `KIND` | `import` or `export` |
| `DESC` | one-line scenario description |
| `LABEL` | the rule label to run, e.g. `//third_party/greeter:greeter` |
| `REPO` | import: upstream slug under `origins/`; export: destination slug |
| `DEST` | subtree to verify (import: the import dest; export: `from_path`) |
| `AGENT` / `AGENT_DESC` | the mock agent's script name / human label for the table |
| `VERIFY` | verifier shell command run in the subtree (e.g. `go test ./...`) |
| `RETRIES` | verify → retry budget for the agent loop (import; default 1) |
| `EXPECT` | expected verifier outcome (`pass`) |
| `SCRUB_FORBID` | export: regex that must **not** appear in the exported tree |

## How the mock agent works

The harness exports `CAPYFUN_AGENT_FIXTURE=<fixture>/agent` and runs the import
with `--executor fixture`. For each `agent_transform`, the engine checks out the
destination subtree to a temp dir and invokes `<name>.sh` (where `<name>` is the
agent label's target) with that dir as CWD. The script edits files in place; the
engine diffs the result into a content-addressed patch.

The script sees the rendered prompt in `$CAPYFUN_AGENT_PROMPT`. When a verifier is
configured (`CAPYFUN_VERIFY`), the agent runs under [`VerifyingRunner`]: after the
edit, the verifier runs; on failure its output is appended to the prompt (so the
script can branch on `VERIFIER FAILED`) and the script runs again, up to `RETRIES`
times. This is how `dependency-modernize` demonstrates a real "fail → feed back →
fix on retry" loop deterministically.

## Adding a fixture

1. `mkdir tests/fixtures/evals/<name>` and write `meta.env`.
2. For an import: drop `commits/01/…`, a `monorepo/` with the import edge and tool
   rules, and `agent/<name>.sh`.
3. For an export: drop a `monorepo/` with the source path and a `github_export`
   edge.
4. Run `scripts/eval-agents.sh <name>`.

[`FixtureRunner`]: ../../../src/engine/agent_exec.rs
[`VerifyingRunner`]: ../../../src/engine/agent_exec.rs
