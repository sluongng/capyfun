# Example: transformations + generative agents (syntax preview)

> **Forward-looking.** This example uses transform and agent builtins that land
> in the milestones in `../../docs/design/transformations.md` (T1–T5). It is a
> syntax preview and is **not** evaluable by `capyfun config` yet. For the
> currently-working config model, see `../monorepo`.

## Layout

```text
examples/transforms/
├── SRC                                  # root: monorepo()
├── tools/
│   ├── harness/SRC                      # harness rules (claude_code, codex, pi)
│   ├── models/SRC                       # model rules (opus, gpt55, nemotron)
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

See `../../docs/design/transformations.md` for the full specification.
