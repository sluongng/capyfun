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
  `monorepo`, `github_import`, `github_export`, `git_repository`, and the agent
  tool rules `harness`, `model`, `agent`, `prompt_template`.
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
    agent = "//tools/agent:reviewer",         # an `agent` rule, by label
    prompt = template(
        "//tools/agent/prompts:review",        # a `prompt_template` rule, by label
        vars = {"style": "//docs:STYLE.md"},
    ),
    paths = ["lib/**"],                        # optional scope; default = whole subtree
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
harness(name = "antigravity", kind = "antigravity")
harness(name = "pi", kind = "pi")

# //tools/models/SRC
model(name = "opus", provider = "anthropic", id = "claude-opus-4-8")
model(name = "gpt55", provider = "openai", id = "gpt-5.5")
model(name = "gemini_flash", provider = "google", id = "Gemini 3.5 Flash (High)")
model(name = "nemotron", provider = "nebius", id = "nvidia/Nemotron-3-Ultra-550b-a55b")

# //tools/agent/SRC
agent(name = "reviewer", harness = "//tools/harness:claude_code", model = "//tools/models:opus")
agent(name = "triager",  harness = "//tools/harness:antigravity", model = "//tools/models:gemini_flash")
agent(name = "porter",   harness = "//tools/harness:codex",       model = "//tools/models:gpt55")
agent(name = "scout",    harness = "//tools/harness:pi",          model = "//tools/models:gpt55")
```

- A **harness** is the agent runtime (Claude Code, Codex, Antigravity, Pi, …). It
  carries runfiles — `plugins` and `skills` — that travel with it.
- A **model** names the LLM (provider + id).
- An **agent** pairs a harness with a model. Multiple agents coexist in one repo;
  models swap under a shared harness, harnesses are reused across agents.

Not every harness can drive every model. `claude_code` runs Anthropic models;
`antigravity` runs Google Gemini models; `codex` and `pi` run OpenAI and other
open models (e.g. NVIDIA Nemotron served by Nebius). An `agent` whose harness
cannot drive its model is a validation error.
See `examples/transforms/tools/{harness,models,agent}/SRC` for a worked set.

The agent's identity for caching is `(harness kind, plugins digests, skills
digests, model provider+id)`.

#### Provisioning (how the runfiles get there)

Harness binaries, plugins, and skills are external artifacts brought in by a
content-addressed fetch rule. The implemented primitive is **`git_repository`**,
which fetches a repo at an exact commit SHA and materializes that snapshot into
its package (no upstream history) — reproducible and inspectable as source:

```python
# //tools/plugins/SRC
git_repository(name = "bazel", repo = "acme/cc-plugin-bazel", commit = "<40-hex>")
```

`harness` references these by label (`plugins = ["//tools/plugins:bazel"]`). An
`http_archive` rule (url + sha256, for released tarball harness binaries) is the
natural follow-on; both follow the Bazel repo-rule pattern (digest-pinned,
cached, materialized). Models carry no artifact — only a provider/id plus a
credential reference resolved at execution time.

### Prompt templating

Each prompt is a first-class **`prompt_template` rule** that wraps a `.tmpl`
file, declared in an SRC file and referenced by label — so prompts are
versioned, reviewable targets like any other rule:

```python
# //tools/agent/prompts/SRC
prompt_template(name = "review",    src = "review.tmpl")
prompt_template(name = "port",      src = "port.tmpl")
prompt_template(name = "modernize", src = "modernize.tmpl")
```

The `template()` value constructor binds a `prompt_template` target to call-site
`vars`. A template references a fixed set of **typed context vars** injected by
the engine at execution time, plus user `vars` (strings or file labels):

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

A no-op re-import still re-applies the tip layer, but every `agent_transform` is
served from the cache (no model call), so the engine reports it explicitly, e.g.
`already up to date (replayed tip from cache: 1 agent, cache 1h/0m)`. This is the
visible cost signal: the expensive model runs only on a cache **miss**.

### The verify → retry loop

An agent transform is verified, not trusted. The engine can wrap the agent runner
in a verify → retry loop:

```text
agent edits → verifier runs → if failed, feed stderr+diff back → retry → materialize final patch
```

After the agent edits the checked-out subtree, a verifier command (e.g. `go test
./...`, `cargo test`) runs in that working directory. On failure, its combined
output is appended to the prompt and the agent runs again, up to a retry budget,
before the import aborts. Crucially, the cache key is computed from the *original*
rendered prompt, so the fed-back feedback never changes cache identity — the
patch that gets materialized is the **verified, final** state, and replays stay
both correct and free. See `VerifyingRunner` in `src/engine/agent_exec.rs`.

### Executors: where an agent runs

Agent execution sits behind one trait (`AgentRunner`), so *where* a transform runs
is a swappable backend chosen per run with `--executor`:

| Executor | Runner | Behavior |
|----------|--------|----------|
| `local` (default) | `LiveRunner` | shells out to the harness CLI in the subtree checkout |
| `remote` | `RemoteRunner` | runs the harness as a REAPI Action on BuildBuddy; Action-Cache–served (see `remote-execution.md`) |
| `fixture` | `FixtureRunner` | a deterministic *mock*: runs a recorded script instead of a model — no model, no network, zero cost |

The fixture executor exists so the full materialize → diff → cache → replay loop
(and the verify → retry loop) can run hermetically in demos, evals, and CI without
model access. It exercises the exact production code path; only the in-workdir edit
is recorded rather than generated. Configured via `CAPYFUN_AGENT_FIXTURE` (script
directory), `CAPYFUN_VERIFY` (verifier command), and `CAPYFUN_VERIFY_RETRIES`. See
`docs/evals.md` and `tests/fixtures/evals/`.

### Credentials

Generative transforms call provider APIs, so they need credentials — but config
evaluation is **pure** (no secrets, no I/O). The rule: a secret value never
appears in an SRC or `.scl` file. A `model` carries at most a credential
*reference* (a name), and the engine resolves it to a value at execution time,
injecting it into the harness subprocess's environment.

- **Default (convention).** With no `credential` field, the provider maps to a
  conventional environment variable, so the common case needs no config — just
  export the variable:

  | provider | env var |
  |---|---|
  | `anthropic` | `ANTHROPIC_API_KEY` |
  | `openai` | `OPENAI_API_KEY` |
  | `nebius` | `NEBIUS_API_KEY` |

- **Override.** `model(..., credential = "env:NAME")` names a specific variable
  (e.g. to select an account). The `env:` prefix is a reference *scheme*;
  `file:` and secret-manager schemes are natural follow-ons. The value is read
  only in the engine, never during config evaluation.

- **Ambient-login fallthrough.** The CLI harnesses (`claude_code`, `codex`,
  `antigravity`) authenticate via their own login (`claude` / `codex login` /
  `agy`, kept in a keyring or the tool's config dir). If no key resolves, the
  engine passes the subprocess environment through rather than clobbering it, so
  the harness uses that login.
  This is why such an agent runs with zero credential config on a machine where
  the CLI is already logged in. The HTTP harness (`pi`) has no login and always
  requires a key.

- **HTTP harness base URL.** `pi` calls an OpenAI-compatible
  `/chat/completions` endpoint directly. Its base URL defaults per provider
  (`nebius` → the Nebius Token Factory endpoint, `openai` → `api.openai.com`)
  and is overridable; the resolved key is sent as a bearer token.

The credential reference is **not** part of the agent cache identity — only
`(harness kind, plugins/skills digests, model provider+id)` is — so rotating a
key or switching accounts does not invalidate materialized agent output.

The engine's execution model for an `agent_transform` is therefore: render the
prompt, resolve the model's credential reference to a value, spawn the harness
CLI with that value in its environment (plugins/skills as runfiles), capture its
edits as a patch. Shelling out to `claude -p "<prompt>"` is the minimal proof of
this path.

## Validation rules

Hard errors:

- unknown transform constructor (falls out of the closed vocabulary);
- `agent_transform.agent` does not resolve to an `agent` rule;
- `template()` does not reference a `prompt_template` target, or a referenced var
  label does not resolve; a `prompt_template.src` file is missing;
- `harness.kind` / `model.provider` outside the known set; empty `model.id`;
- `model.credential` that is not a recognized reference scheme (`env:<NAME>`
  with a non-empty name); note this validates the *reference shape* only —
  whether the variable is actually set is an execution-time concern, not a
  config error;
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
- **T5 — Generative execution + cache. _(done)_** `capyfun import` now executes
  the tip layer: it applies `apply_patch` transforms and runs `agent_transform`s
  in declared order as `CapyFun-Patch` / `CapyFun-Agent` commits on top of the
  mirror. Each agent runs in a temporary checkout of the destination subtree; its
  edits are captured as a unified diff in pure libgit2 (diff the edited temp tree
  against the original subtree), and that **materialized patch** is stored under a
  content-addressed key (blake3 of `(parent subtree OID, rendered prompt, agent
  identity)`) at `<repo>/.git/capyfun/agent-cache/<key>.patch`. A cache hit (the
  default on re-import) replays the recorded patch deterministically; `--refresh`
  bypasses the cache and regenerates. An agent run that makes no edits creates no
  commit. The harness is injected behind an `AgentRunner` trait (production:
  shell out to the CLI with the temp dir as CWD; tests: a deterministic fake), so
  the whole loop is exercised hermetically with no network. The credential
  reference is **not** part of the cache identity, so rotating a key does not
  invalidate output. Started with the CLI harnesses (`claude_code`/`codex`/
  `antigravity`); `pi` (HTTP, returns text only) is rejected as a file-editing
  runner.

## Open questions

- Should `replace`/`move` ever be available as tip transforms (explicit phase
  override), or is phase-by-type sufficient?
- How is `repo_context` selected for `agent_transform` — whole subtree, changed
  files plus neighbors, or a declared include set? _(T5 fills it best-effort with
  the destination subtree's file list; `changed_files` is the same list and
  `incoming_diff` is the newest mirror commit's first-parent diff. A precise
  include-set selection driven by the transform's `paths` scope is still open.)_
- How are harness/model versions pinned and surfaced in the cache key (image
  digest, CLI version, API snapshot)? _(T5's identity is `(harness kind, model
  provider+id)`; plugins/skills digests and CLI/API versions are not yet folded
  in — the `HarnessKind` carries no runfiles digest today.)_
- ~~How are provider credentials supplied without putting secrets in pure
  config?~~ **Resolved:** config carries an `env:`-scheme credential *reference*
  (default = provider→conventional env var), resolved by the engine at execution
  time; `claude_code` falls through to ambient CLI login. See *Credentials*.
- ~~Where is the materialized-patch / agent-output store kept (in-repo under a
  CapyFun dir, or a separate content-addressed store keyed in the commit map)?~~
  **Resolved (T5):** under the repo's git dir at
  `<repo>/.git/capyfun/agent-cache/<key>.patch`, keyed by a content-addressed
  blake3 of `(parent subtree OID, rendered prompt, agent identity)`. (Whether
  this should also be mirrored into a shareable/committed store is still open.)
- Do generative transforms need a sandbox/network policy at execution time?
