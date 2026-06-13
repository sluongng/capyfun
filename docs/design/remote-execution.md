# CapyFun Remote Execution: agent transforms on RBE (BuildBuddy)

This document specifies how CapyFun offloads generative (`agent_transform`)
execution to a Remote Execution API (REAPI) backend — concretely **BuildBuddy
Cloud** — so agent runs are sandboxed, fan out across a worker pool, and are
cached by the **Action Cache** instead of CapyFun's local content-addressed
store. It is the concrete form of the "distributed agent-farm runtime" noted in
the README and the "run reconciles in a sandbox" safety requirement in
`automation.md`.

Read `../../CLAUDE.md` and `transformations.md` first. This is a design/roadmap
artifact; the buildable first slice (R0–R3) is spec'd at the end.

> **Status:** not started. The local execution path
> (`src/engine/agent_exec.rs`, `LiveRunner` + the blake3 content-addressed
> cache) is the baseline this replaces/augments. Scope decided for the first
> pass: **Tier 1** (one `agent_transform` → one REAPI Action) against
> **BuildBuddy Cloud**, with the **Action Cache as the agent-output cache**.

## Thesis: an `agent_transform` *is* a REAPI Action

CapyFun's transform cache and REAPI's action cache are the same shape, so the
mapping is structural, not forced:

| CapyFun (today, local) | REAPI / BuildBuddy |
|---|---|
| `agent_transform`: subtree + prompt + harness/model + runfiles → patch | an **Action**: `input_root` + `Command` → `ActionResult` |
| JOSH-style cache `(input object, filter) → output object` | action cache `(Command + input_root + platform digest) → ActionResult` |
| blake3 content-addressed agent-output record (`agent_exec.rs`) | CAS, addressed by the patch blob's digest |
| `AgentInvocation::identity()` (harness+provider+model; **excludes** credential) | the Action digest (must likewise exclude the API key) |
| `git_repository` vendoring harnesses/plugins/skills as pinned snapshots | the **`input_root` / runfiles** of the Action |

The load-bearing consequence (the reason to do this): if the patch is declared
as the Action's **`output_paths`**, then the `ActionResult` stored in the Action
Cache — keyed by the Action digest — carries the patch blob's CAS digest. A
`GetActionResult(action_digest)` **hit is the agent-output cache lookup**: the
patch comes back without ever running the agent. The Action digest already
captures subtree + prompt + harness + model + runfiles, so CapyFun's local
blake3 cache collapses into the AC→CAS path — and becomes shared across the team
and CI instead of being per-checkout.

This realizes, for free, the invariant in `transformations.md`: generative
output is "materialized into a content-addressed record so imports remain
reproducible from that record." On RBE the record *is* the AC entry.

## What this is NOT

- **Not turning CapyFun into Bazel.** CapyFun keeps its own narrow Starlark and
  its own engine; only the opaque, expensive transform *step* is dispatched as a
  REAPI Action. `git2` plumbing and the mirror/tip replay stay local.
- **Not adopting `buildbuddy-io/rules_claude` wholesale** — that ruleset runs
  Claude Code as a *Bazel* action and would pull us into Bazel. We use it as
  **prior art and reference**: it proves the hermetic-Claude-action pattern and
  contributes one technique we borrow (below).
- **Not remote execution of structural transforms or git plumbing** in the first
  pass. Those are cheap and deterministic locally; revisit only if profiling
  says so.

## Borrowed from `rules_claude`: pin the harness as runfiles

`rules_claude` downloads the Claude Code binary with SHA256 verification (a
hermetic, pinned toolchain). CapyFun borrows the *idea*, not the rules: vendor
the pinned harness binary (and any plugins/skills) into the Action's
`input_root` via the existing `git_repository` snapshot primitive — the
"provisioning primitive for external tools." This keeps runfiles
content-addressed and folded into the Action digest, and avoids managing a
container image. (Folding runfiles into the cache identity also closes the
`AgentInvocation::identity()` TODO that notes plugins/skills are "not yet folded
in.")

## Action structure for one `agent_transform`

1. **`input_root`** (a REAPI `Directory` Merkle tree): the destination subtree at
   the commit being transformed + the rendered prompt + the vendored harness
   runfiles. Built by serializing the git tree to REAPI `Directory`/`FileNode`
   protos (mapping git mode/exec-bit/symlink → `NodeProperties`), then
   `FindMissingBlobs` + `BatchUpdateBlobs` to upload only what the CAS lacks.
2. **`Command`**: `arguments` = the harness invocation (the `harness_command`
   CapyFun already builds for `LiveRunner`); `output_paths = ["out.patch"]`;
   `platform` properties for the executor pool, container, and **network
   egress** (agents must reach the model API — see Safety).
3. **`Action`**: `command_digest` + `input_root_digest` + `platform` + `timeout`,
   `do_not_cache = false` so it populates the AC.
4. **Execute / cache**: `GetActionResult(action_digest)` first; on hit, fetch the
   `out.patch` blob from CAS and feed it into the existing tip-layer apply path.
   On miss, `Execute()` (the server populates the AC), then fetch the patch.
   `--no-cache` maps to `skip_cache_lookup` / re-running with a salt.

## Integration seam

`src/engine/agent_exec.rs` already abstracts execution behind the `AgentRunner`
trait (`LiveRunner` local shell-out; a fake runner in tests). Two integration
depths:

- **Minimal** — add a `RemoteRunner: AgentRunner` that runs the Action and
  writes the resulting files into `workdir`, leaving the engine's existing
  diff→materialize→cache loop unchanged. Gets remote *execution* and sandboxing,
  but still uses the local blake3 cache.
- **Full (the goal)** — the patch is the Action's `output_paths`, so the
  `GetActionResult` check must happen *above* the trait, at the engine's
  cache-check site (where the blake3 lookup lives in `engine.rs`). On an AC hit
  the agent never runs; the cached patch is applied directly. This is what makes
  "the Action Cache *is* the agent-output cache" real.

Start at Minimal to land a remote round-trip, then lift the cache check to Full.

## Safety

- **Credential out of the cache key.** The model API key must not ride in
  `Command.environment_variables` (it would enter the Action digest, leak into
  the key, and bust every entry on rotation). Pass it via a BuildBuddy
  secret / platform header instead. This mirrors `identity()` already excluding
  the credential. *(Open question: exact BuildBuddy secret mechanism.)*
- **Network policy.** Agent actions are *not* hermetic — they need egress to the
  model API, which classic RBE forbids. BuildBuddy supports actions with network
  access; scope egress to the model endpoint via platform properties.
- **Nondeterminism → record-and-replay.** Agents are nondeterministic, but
  caching by Action digest pins the *first* run's patch as canonical. The
  durable record of last resort remains CapyFun's own committed patch (the
  commit map is the source of truth); the AC is an accelerator, so AC eviction
  degrades to a re-run, not corruption.
- **Auth to BuildBuddy:** gRPC to `remote.buildbuddy.io` with an API key header.

## Milestones (R-series)

Mirrors the M/T discipline: small, runnable, tested. Network-touching steps are
gated behind an env-keyed integration test so `cargo test` stays hermetic.

### R0 — REAPI client + CAS round-trip
- New `remote` module: `tonic` + the `bazelbuild/remote-apis` protos. Connect to
  BuildBuddy Cloud; `FindMissingBlobs` / `BatchUpdateBlobs` / read a blob back.
- Acceptance: upload a blob, fetch it by digest from Cloud (integration test
  gated on a `BUILDBUDDY_API_KEY` env var; skipped otherwise).

### R1 — git tree → REAPI `input_root` + Action digest
- Serialize a git subtree to a REAPI `Directory` Merkle tree (mode/exec/symlink
  mapping); assemble `Command` (output_paths = the patch) + `Action`; compute the
  Action digest.
- Acceptance: deterministic Action digest for fixed inputs — a pure, offline
  golden test (no network).

### R2 — Execute() + fetch the patch (Minimal `RemoteRunner`)
- `RemoteRunner: AgentRunner` calls `Execute`, fetches `out.patch` from CAS,
  applies it through the existing tip-layer path. Harness binary + prompt +
  runfiles ride in `input_root` (pinned-snapshot trick).
- Acceptance: an `agent_transform` import executed remotely produces a patch
  equivalent to the local path on the same fixture.

### R3 — Action Cache as the agent-output cache (Full)
- Lift the cache check to the engine: `GetActionResult(action_digest)` before
  Execute; on hit, skip the agent entirely and apply the cached patch. Wire
  `--no-cache` → `skip_cache_lookup`.
- Acceptance: re-importing an unchanged `agent_transform` is an AC hit (no agent
  run); changing prompt/subtree/model/runfiles busts it.

### R4 — Backend selection + reconcile fan-out
- `--executor=local|remote` (config/env) in the engine; the reconciler dispatches
  agent_transforms across affected targets concurrently to the remote pool, with
  clean fallback to local.
- Acceptance: a multi-target reconcile fans out remotely; remote-unavailable
  falls back to `LiveRunner`.

## Open questions

- **BuildBuddy secret mechanism** for the model API key so it stays out of the
  Action digest — platform header, BuildBuddy secrets, or a sidecar?
- **Network egress policy** granularity on BuildBuddy actions (per-endpoint
  allowlist vs all-or-nothing).
- **Directory serialization fidelity:** git ↔ REAPI mapping for exec bits,
  symlinks, and empty dirs; does the round-trip preserve the tree OID on
  re-import?
- **Harness availability on executors:** vendored pinned binary in `input_root`
  (preferred, in-grain) vs a prebuilt executor image — which is more robust for
  Claude Code / Codex / `pi`?
- **REAPI proto sourcing in Rust:** generate from `bazelbuild/remote-apis` via
  `tonic-build`, or an existing crate? Pin the API version.
- **Reproducibility vs freshness:** when (if ever) should a reconcile bypass the
  AC to let an agent re-run against newer upstream context, given record-and-
  replay otherwise pins the first patch forever?
</content>
</invoke>
