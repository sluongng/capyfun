# Example: transformations + generative agents

> **Evaluable today.** The transform and agent builtins (`replace`, `move`,
> `copy`, `rewrite_message`, `apply_patch`, `agent_transform`/`template`, and the
> `harness`/`model`/`agent`/`prompt_template` rules) are implemented (T1–T4), so
> the whole tree compiles and statically validates:
>
> ```sh
> capyfun check --root examples/transforms
> ```
>
> Structural transforms (`replace`/`move`/`copy`/`rewrite_message`) also apply
> per commit during `capyfun import`. The one remaining milestone is generative
> **execution** (T5): actually running an `agent_transform` and materializing its
> output to a content-addressed patch. See
> `../../docs/design/transformations.md`.

## Layout

```text
examples/transforms/
├── SRC                                  # root: monorepo()
├── materialize-widget.sh                # hermetic runnable import (see below)
├── widget-origin/                       # fixture upstream for acme/widget
├── tools/
│   ├── harness/SRC                      # harness rules (claude_code, codex, antigravity, pi)
│   ├── models/SRC                       # model rules (opus, gpt55, gemini_flash, nemotron)
│   ├── plugins/SRC                      # git_repository: vendored plugin snapshots
│   ├── skills/SRC                       # git_repository: vendored skill snapshots
│   └── agent/
│       ├── SRC                          # agent rules (harness + model pairings)
│       └── prompts/
│           ├── SRC                      # prompt_template targets (review/port/modernize)
│           ├── review.tmpl
│           ├── port.tmpl
│           └── modernize.tmpl
└── third_party/
    └── widget/
        ├── SRC                          # github_import with a transform pipeline
        └── patches/0001-pin-toolchain.patch
```

Harnesses, models, and agents live in sibling packages, so an agent reads as
`//tools/agent:reviewer` pointing at `//tools/harness:claude_code` and
`//tools/models:opus`. A harness can only drive models it supports.

## What it demonstrates

A single `github_import` with a transform pipeline spanning both phases:

- **Structural (mirror-time, per replayed commit):** `replace` scrubs internal
  references across all history, `move` relocates `pkg/` → `lib/`, and
  `rewrite_message` strips an internal trailer from every commit. The mirror
  stays faithful (each commit maps to its origin) but is consistently rewritten.
- **Local-modification (tip, once on top):** `apply_patch` pins the toolchain,
  and two `agent_transform`s run coding agents — `//tools/agent:porter` with the
  `//tools/agent/prompts:port` template, then `//tools/agent:modernizer` with
  `//tools/agent/prompts:modernize`. Prompt templates are first-class targets
  (`prompt_template`), and each agent's output is materialized to a
  content-addressed patch so the import stays reproducible.

Agents are composable tool dependencies: a `harness` (runtime + plugins/skills
runfiles) plus a `model` (LLM), paired into an `agent` and referenced by label.

## Materializing widget (runnable today)

The `transforms = [...]` pipeline above is a syntax preview, but you can still
see `third_party/widget` *materialized* end-to-end against a real fixture:

```sh
examples/transforms/materialize-widget.sh
```

It builds a local **bare** repo from `widget-origin/` (with a couple of commits
of history), runs the real `capyfun import` against it via
`CAPYFUN_GITHUB_BASE`, and prints the resulting tree and history — first-parent
mirror commits tagged `CapyFun-Origin`, plus the toolchain patch applied on top
as a `CapyFun-Patch` tip commit. It is hermetic (no network) and rerunnable.

This exercises the **implemented plain-mirror path** (`github_import` +
`patches`); the structural and generative transforms in `widget/SRC` are the
forward-looking T1–T5 work and are not applied by the script. An import's real
artifact is git history, so the materialized content lives as commits in the
script's throwaway monorepo rather than as files checked in here.

## Running the agents

`run-agents.sh` smoke-runs the agents from `tools/agent/SRC` through
`capyfun agent-run`:

```sh
examples/transforms/run-agents.sh
```

- **reviewer** (`claude_code` + opus), **triager** (`antigravity` + gemini_flash),
  and **porter** (`codex` + gpt55) use the local `claude` / `agy` / `codex` CLI
  logins — no key needed.
- **modernizer** (`pi` + nemotron) calls Nebius's OpenAI-compatible endpoint and
  needs `NEBIUS_API_KEY`; it is skipped if that is absent.

You can also drive a single agent directly, e.g.:

```sh
capyfun agent-run --harness pi --provider nebius \
    --model nvidia/Nemotron-3-Ultra-550b-a55b --prompt "Summarize this diff."
```

## Credentials

Secrets never live in config (config evaluation is pure). A `model` carries at
most a credential *reference*; the engine resolves it at execution time and
injects it into the harness. By default the provider maps to a conventional env
var (`anthropic` → `ANTHROPIC_API_KEY`, `openai` → `OPENAI_API_KEY`, `nebius` →
`NEBIUS_API_KEY`), so the common case is just exporting the variable. The CLI
harnesses (`claude_code`, `codex`) fall through to their own login when no key is
set; the `pi` HTTP harness always needs a key.

For local testing, export the variable or put it in `secrets.env` next to this
README (gitignored), which `run-agents.sh` loads automatically:

```sh
echo 'NEBIUS_API_KEY=...' > examples/transforms/secrets.env
```

See the comments in `tools/models/SRC` and the *Credentials* section of
`../../docs/design/transformations.md` for the full specification.
