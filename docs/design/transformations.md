# CapyFun Transformations

CapyFun can transform code as it moves across a tree boundary. This document
specifies the transformation system: what transforms exist, when they apply, and
how the generative (coding-agent) transforms stay reproducible.

Read `../../CLAUDE.md` first for the project thesis and the SRC/`.scl` config
model. This doc assumes the import mirror/patch model already described there.

## Principles (and how they reconcile with the invariants)

CapyFun's invariants say "narrow Starlark, not a general filter DSL" and
"rewriting is deterministic." Transformations evolve those invariants without
discarding them:

- **Closed, typed vocabulary.** Transforms are a fixed set of typed builtins
  (like Copybara's `core.*`), not arbitrary user code. The config *declares* a
  pipeline drawn from a known vocabulary, so the IR stays fully introspectable
  and statically validatable. Adding a transform means adding a builtin, not
  opening a code-execution surface.
- **Determinism, preserved for the record.** Imperative transforms are
  deterministic. Generative transforms are nondeterministic *at generation
  time*, but their output is **materialized into a content-addressed record**, so
  an import is always reproducible from that record. Regeneration is explicit.
- **Pure config evaluation.** Constructing a transform in config does no I/O.
  Transforms execute only in the engine.

## Rules vs. value constructors

The config has two kinds of typed builtins:

- **Rules** are named, instantiated only in SRC files, and recorded into the IR:
  `monorepo`, `github_import`, `github_export`, and the agent tool rules
  `harness`, `model`, `agent`.
- **Value constructors** are pure functions that *return a typed value* and may
  be used anywhere (including `.scl` libraries, e.g. to define reusable
  transform constants): the transform constructors `replace`, `move`, `copy`,
  `apply_patch`, `rewrite_message`, `agent_transform`, and the `template`
  helper. They record nothing, so the "no top-level rule instantiation in a
  library" guard does not apply to them.

Transforms attach to an import or export via an ordered `transforms = [...]`
list:

```python
github_import(
    name = "widget",
    repo = "acme/widget",
    transforms = [
        replace(before = "acme.internal/", after = "", paths = ["**/*.go"]),
        move(src = "pkg", dst = "lib"),
        rewrite_message(strip_trailers = ["Internal-Review"]),
        apply_patch("patches/0001-pin-toolchain.patch"),
        agent_transform(agent = "//tools/agent:reviewer", prompt = template(...)),
    ],
)
```

The existing `patches = [...]` field is sugar for appending `apply_patch(...)`
transforms to the tip phase (see below).

## Two phases

Each transform has an inherent **phase**, decided by its kind. The engine groups
the pipeline by phase (preserving order within a phase); the mirror phase runs
before the tip phase.

### Mirror-time (structural) — applied per commit

`replace`, `move`, `copy`, `rewrite_message` are **structural**: they are applied
to *every* replayed commit as the first-parent mirror is built, exactly like
git-filter-repo rewriting trees/messages across history. The result:

```text
MIRROR (per-commit structural rewrite)
* c3' relocate+rewrite of c3   CapyFun-Origin: c3
* c2' relocate+rewrite of c2   CapyFun-Origin: c2
* c1' relocate+rewrite of c1   CapyFun-Origin: c1
```

The mirror stays *faithful in the structural sense*: deterministic, and every
commit still maps 1:1 to an origin commit via `CapyFun-Origin`. It is not
byte-identical to upstream — it is upstream consistently relocated/scrubbed.

The imported result is tracked in version control like any other monorepo
content, so CapyFun does not try to detect or auto-rebuild a mirror when a
structural transform definition changes. Reconciling an already-imported history
with a changed transform set is a separate "evolve the changeset" concern (cf.
the Mercurial evolution extension) and is **out of scope** — import only ever
appends new upstream commits onto the existing mirror.

### Tip (local-modification) — applied once, on top

`apply_patch` and `agent_transform` are **local modifications**: after the mirror
reaches the new upstream tip, they are applied in order as CapyFun-authored
commits on top, each tagged with a trailer (`CapyFun-Patch: <file>` or
`CapyFun-Agent: <id>`):

```text
TIP (rebased local mods)
* p2 [agent] generated change  CapyFun-Agent: <id>
* p1 [patch] local fix         CapyFun-Patch: patches/0001-...
```

On an incremental re-import, new upstream commits extend the mirror and the tip
layer is dropped and re-applied (rebased) onto the new mirror tip. A transform
that fails to apply aborts the import; nothing is half-written.

## Imperative transforms

All paths are relative to the imported subtree (upstream-shaped), so a transform
is portable and could be sent upstream where that makes sense.

| Constructor | Phase | Purpose | Key fields |
|---|---|---|---|
| `replace` | mirror | sed-like search/replace across matching files | `before`, `after`, `paths` (globs), `regex=False` |
| `move` | mirror | relocate a file or directory | `src`, `dst` |
| `copy` | mirror | duplicate a file or directory | `src`, `dst` |
| `rewrite_message` | mirror | rewrite commit messages | `before`/`after`/`regex`, `strip_trailers`, `add_trailers` |
| `apply_patch` | tip | apply a static unified-diff patch file | positional `file` |

Semantics notes:

- `replace` operates on blob contents of files matching `paths`. With
  `regex=True`, `before` is a regular expression and `after` may reference
  capture groups. Deterministic and applied to every commit.
- `move`/`copy` operate on tree entries. Applied per commit, so the layout is
  consistent throughout the mirrored history (not a single rename commit at the
  tip).
- `rewrite_message` edits the commit message text/trailers per commit. The
  `CapyFun-Origin` trailer is always preserved/appended by the engine.
- `apply_patch` is a tip transform; the patch applies within the (already
  structurally-transformed) subtree.

## Generative transforms

`agent_transform` runs a coding agent over the current code plus the incoming
change, and captures the agent's edits as a patch in the tip layer.

```python
agent_transform(
    agent = "//tools/agent:reviewer",      # an `agent` rule, by label
    prompt = template(
        "//tools/agent/prompts:port.tmpl",
        vars = {"style": "//docs:STYLE.md"},
    ),
    paths = ["lib/**"],                     # optional scope; default = whole subtree
)
```

### Agents as tool dependencies

Agents are modeled as composable tool rules, declared in a tools package and
referenced by label:

Harnesses, models, and agents live in sibling packages so labels read clearly:

```python
# //tools/harness/SRC
harness(name = "claude_code", kind = "claude_code",
        plugins = ["//tools/plugins:bazel"], skills = ["//tools/skills:review"])
harness(name = "codex", kind = "codex")
harness(name = "pi", kind = "pi")

# //tools/models/SRC
model(name = "opus", provider = "anthropic", id = "claude-opus-4-8")
model(name = "gpt55", provider = "openai", id = "gpt-5.5")
model(name = "nemotron", provider = "nebius", id = "nvidia/nemotron-3-ultra")

# //tools/agent/SRC
agent(name = "reviewer", harness = "//tools/harness:claude_code", model = "//tools/models:opus")
agent(name = "porter",   harness = "//tools/harness:codex",       model = "//tools/models:gpt55")
agent(name = "scout",    harness = "//tools/harness:pi",          model = "//tools/models:gpt55")
```

- A **harness** is the agent runtime (Claude Code, Codex, Pi, …). It carries
  runfiles — `plugins` and `skills` — that travel with it.
- A **model** names the LLM (provider + id).
- An **agent** pairs a harness with a model. Multiple agents coexist in one repo;
  models swap under a shared harness, harnesses are reused across agents.

Not every harness can drive every model. `claude_code` runs Anthropic models;
`codex` and `pi` run OpenAI and other open models (e.g. NVIDIA Nemotron served by
Nebius). An `agent` whose harness cannot drive its model is a validation error.
See `examples/transforms/tools/{harness,models,agent}/SRC` for a worked set.

The agent's identity for caching is `(harness kind, plugins digests, skills
digests, model provider+id)`.

### Prompt templating

Prompts compose from template files via the `template()` value constructor.
A template references a fixed set of **typed context vars** injected by the
engine at execution time, plus user `vars` (strings or file labels):

| Context var | Meaning |
|---|---|
| `{{incoming_diff}}` | the change being imported (new upstream commits / diff) |
| `{{changed_files}}` | list of paths touched by the incoming change |
| `{{repo_context}}` | relevant existing code in the destination subtree |
| `{{origin_commit}}` / `{{origin_message}}` | origin metadata |

Templates may include/compose other templates. Rendering is pure given its
inputs; only the agent run itself is nondeterministic.

### Materialization and the content-addressed cache

To keep the record reproducible:

1. The engine renders the prompt and assembles inputs (subtree state + incoming
   change).
2. It computes a cache key:
   `H(input subtree OID, incoming change, rendered prompt, agent identity)`.
3. **Cache hit** → replay the recorded patch deterministically.
4. **Cache miss** (or explicit `--refresh`) → run the agent, capture its edits as
   a patch, store it under the cache key, and apply it.

The materialized patch is the durable artifact; it is what gets committed (with a
`CapyFun-Agent` trailer) and what an incremental import replays. Thus an import
is reproducible from the cache even though generation is not.

## Validation rules

Hard errors:

- unknown transform constructor (falls out of the closed vocabulary);
- `agent_transform.agent` does not resolve to an `agent` rule;
- `template()` path does not resolve, or a referenced var label does not resolve;
- `harness.kind` / `model.provider` outside the known set; empty `model.id`;
- an `agent` whose harness cannot drive its model (e.g. `claude_code` paired with
  an OpenAI model);
- `move`/`copy`/`replace`/`apply_patch` paths that escape the subtree, are
  absolute, or contain `..`;
- a structural transform listed where only tip is valid, or vice versa (only
  possible if a future transform allows an explicit phase override).

Warnings:

- `replace` whose `paths` match nothing;
- `agent_transform` with no `paths` scope on a large subtree (cost);
- a tip transform placed before structural transforms in the list (reordered by
  phase; harmless but possibly surprising).

## Milestones (transformations)

These build on the import mirror (`docs/plans/import-roadmap.md`, M3–M5). Do not
start them before the plain mirror round-trip works.

- **T1 — Transform values + IR.** Typed transform value constructors and the
  `transforms = [...]` field; normalize into IR with phase tagging; validation.
- **T2 — Structural transforms (mirror-time).** `move`/`copy`/`replace`/
  `rewrite_message` applied per replayed commit; new upstream commits append to
  the existing mirror.
- **T3 — Tip transform layer.** Generalize the patch layer to an ordered tip
  pipeline of `apply_patch` (and later `agent_transform`) commits.
- **T4 — Agent tool rules.** `harness`/`model`/`agent` rules, runfiles
  (plugins/skills) resolution, label references; the `template()` engine and
  typed context vars.
- **T5 — Generative execution + cache.** Run an agent over inputs, materialize
  output to a patch, content-addressed cache, `--refresh`; `CapyFun-Agent`
  trailer. Start with one harness (Claude Code) and one model.

## Open questions

- Should `replace`/`move` ever be available as tip transforms (explicit phase
  override), or is phase-by-type sufficient?
- How is `repo_context` selected for `agent_transform` — whole subtree, changed
  files plus neighbors, or a declared include set?
- How are harness/model versions pinned and surfaced in the cache key (image
  digest, CLI version, API snapshot)?
- Where is the materialized-patch / agent-output store kept (in-repo under a
  CapyFun dir, or a separate content-addressed store keyed in the commit map)?
- Do generative transforms need a sandbox/network policy at execution time?
