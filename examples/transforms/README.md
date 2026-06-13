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
│   └── agents/
│       ├── SRC                          # harness/model/agent tool rules
│       └── prompts/port.tmpl            # prompt template (context vars)
└── third_party/
    └── widget/
        ├── SRC                          # github_import with a transform pipeline
        └── patches/0001-pin-toolchain.patch
```

## What it demonstrates

A single `github_import` with a transform pipeline spanning both phases:

- **Structural (mirror-time, per replayed commit):** `replace` scrubs internal
  references across all history, `move` relocates `pkg/` → `lib/`, and
  `rewrite_message` strips an internal trailer from every commit. The mirror
  stays faithful (each commit maps to its origin) but is consistently rewritten.
- **Local-modification (tip, once on top):** `apply_patch` pins the toolchain,
  and `agent_transform` runs a coding agent (`//tools/agents:reviewer`) with a
  templated prompt to port the change to monorepo conventions. The agent's output
  is materialized to a content-addressed patch so the import stays reproducible.

Agents are composable tool dependencies: a `harness` (runtime + plugins/skills
runfiles) plus a `model` (LLM), paired into an `agent` and referenced by label.

See `../../docs/design/transformations.md` for the full specification.
