# CapyFun Per-Target Reconcile Refs + Monorepo PRs

This document specifies how a reconcile should deliver its result: to a
**per-target ref** and a **monorepo pull request**, rather than by writing the
monorepo's default branch directly.

Read `../../CLAUDE.md` first for the project thesis, and `automation.md` for the
level-triggered reconciler this builds on.

> **Status:** design only — not implemented. Today `capyfun reconcile` and the
> server's `ReconcileActor` advance `refs/heads/<default_branch>` in place (a
> mutex serializes concurrent writers). This doc is the plan to move that output
> to `refs/capyfun/<label>` + a reviewable PR. No code yet.

## Why

Three problems with writing the default branch directly:

1. **It bypasses the monorepo's own review.** Export already respects the
   *destination's* review by opening a PR and never pushing its default branch
   (a core invariant). Import/vendor should respect the *monorepo's* review
   symmetrically: an automated import of arbitrary upstream content — run through
   transforms, possibly an `agent_transform` — is exactly the kind of change a
   human or policy should approve before it lands on mainline. The Safety section
   of `automation.md` already says "default to PR-for-review, not auto-merge, for
   anything that runs transforms."
2. **Concurrent reconciles contend.** The current mutex serializes writers, but
   every target still lands on one branch, so reconciles are coupled and a slow
   one blocks the rest. One ref per target removes the coupling.
3. **It resolves the buried-patch-layer limitation.** `transformations.md` notes
   that re-importing a dependency whose tip-layer commits sit *underneath*
   another import's commits cannot rebase cleanly (`strip_tip_layer` stops at a
   foreign import's commits). When each target owns `refs/capyfun/<label>`, its
   mirror+tip layer is always self-contained at that ref's tip, so the rebase is
   always well-defined. This is the structural fix that doc defers to "use
   per-import refs."

## Model

```
desired (upstream) ─┐
                    ├─► reconcile ─► refs/capyfun/<label>  ─► monorepo PR ─► (human/policy merge) ─► refs/heads/<default>
commit map (actual)─┘                (target's own branch)     (Checks)
```

- A reconcile computes the target's tip exactly as today (mirror + tip layer for
  import; snapshot for vendor), but the new commits are based on the **merged
  state** (the default branch tip) and written to `refs/capyfun/<label>`, not the
  default branch.
- CapyFun then **opens or refreshes** a monorepo PR from that ref into the
  default branch, titled per target ("Import acme/backend: 5 new commits"), with
  status reported via the Checks API.
- **Merging the PR** is what lands the change on mainline. CapyFun never pushes
  the default branch itself — the same contract export already honors for
  destinations.

### Label → ref name

`//third_party/backend:backend` → strip `//`, replace `:` with `/` →
`refs/capyfun/third_party/backend/backend`. The label is already validated to be
path-shaped with non-empty, `..`-free components, so the derived ref is
ref-safe by construction. (A tiny sanitizer rejects/escapes anything that would
violate `git check-ref-format` — e.g. a name ending in `.lock`.)

## The commit-map interaction (the hard part)

The commit map (`CapyFun-Origin` / `CapyFun-Vendor` trailers) is the source of
truth for *actual* state. Two states now carry trailers: the merged default
branch, and the in-flight `refs/capyfun/<label>` of an open PR.

- **Desired-vs-actual** is still computed against the **merged** state (the
  default branch), so `status` is unchanged: it answers "how far is mainline
  behind upstream?"
- **Re-reconciling while a PR is open** must *update* `refs/capyfun/<label>` in
  place, not stack a second PR. The reconcile recomputes the target tip
  deterministically from the merged base + the full upstream delta and
  force-updates the ref with `--force-with-lease`. Because the mirror/tip
  computation is deterministic, an unchanged upstream reproduces the same ref tip
  (idempotent); new upstream commits extend it.
- **Base selection:** the per-target ref is based on the current default-branch
  tip. If mainline moved (an unrelated target merged) since the ref was written,
  the next reconcile rebases this target onto the new base. Per-target isolation
  means that rebase only ever replays *this* target's commits.

## Direct-to-branch vs. PR: who does which

Direct-to-branch is convenient for local/demo and hermetic tests; PR-for-review
is the automation contract. Proposal:

- `capyfun import` / `vendor` / `export` (the explicit, single-shot commands)
  keep their current direct behavior by default — they are developer tools.
- `capyfun reconcile` and `serve` default to **ref + PR** (the automation path),
  with `--write-branch` to opt back into direct writes for local runs.
- Trusted read-only mirrors may auto-merge by policy (noted in `automation.md`).

This keeps the `reconcile::do_*` core unchanged and adds a delivery choice on top
(where the computed tip is written, and whether a PR is opened).

## Invariants (proposed)

- A reconcile never writes `refs/heads/<default_branch>` directly; it writes
  `refs/capyfun/<label>` and opens/refreshes a PR. (Mirrors export's "never push
  the destination default branch.")
- Exactly one ref and one open PR per target; reconciles of distinct targets
  never contend.
- Desired-vs-actual is computed against the merged state; the per-target ref is
  recomputed deterministically, so re-reconciling an open PR updates it in place
  (force-with-lease) and is idempotent.
- The commit map remains the source of truth; the ref/PR are delivery, not state.

## Milestones

- **R1 — Ref derivation + write.** Derive `refs/capyfun/<label>`; have
  `reconcile`/`serve` write the computed tip there instead of the default branch
  (behind the default-on automation path; `--write-branch` for the old behavior).
  Hermetic test: a reconcile leaves the default branch untouched and the ref
  pointing at the new tip.
- **R2 — Monorepo PR open/refresh.** Open a PR from the ref into the default
  branch (gh shell-out first, like export's `open_pr`; `octocrab` later),
  idempotent by locating an existing PR for the head branch. Skipped/printed for
  local destinations, exactly as export does.
- **R3 — Checks status.** Report reconcile status (up to date / N behind /
  applied) via the Checks API.
- **R4 — Open-PR rebase semantics.** Handle base movement and in-flight refs:
  rebase onto the new default-branch tip, force-with-lease, and prove
  re-reconcile-while-open updates one PR rather than spawning a second.

## Open questions

- **Merged-state scan with an open PR:** confirm the trailer scan for "last
  imported" reads the merged branch, not the ref, so a long-open PR does not make
  the reconciler think work already landed.
- **Stale refs:** when a PR is closed without merging, should the next reconcile
  recreate it, or back off? (Probably recreate — level-triggered — with a note.)
- **Many small targets:** one PR per target is a lot of PRs for a big `go.mod`
  sweep. A batched "umbrella" PR mode may be wanted; keep it out of v0.
