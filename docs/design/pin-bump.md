# CapyFun Pin-Bump PRs

This document specifies how CapyFun proposes **upgrading a pinned dependency** —
opening a PR that edits the pin in config when a newer upstream release appears.

Read `../../CLAUDE.md` first for the thesis and the SRC/`.scl` config model, and
`automation.md` for the reconciler and per-rule trigger semantics this extends.

> **Status:** design only — not implemented. Today reconcile converges a pinned
> `git_repository` (and tag-pinned `github_import`) to the *declared* pin; it
> never proposes a *newer* one. `status` likewise compares only the declared pin
> to what is vendored. This doc is the plan for proposing the bump. No code yet.

## The two kinds of "behind"

A history-tracking `github_import` and a pinned dependency are behind in
different senses, and CapyFun must not conflate them:

- **History-tracking import** (`ref = refs/heads/main`): "behind" means new
  first-parent commits exist upstream. Reconcile imports them directly — the SRC
  is unchanged. (Already implemented.)
- **Pinned** (`git_repository` with `commit = …`, or a tag-pinned import):
  "behind" means a newer *release/tag* exists than the one the SRC pins.
  Upgrading is a **config edit** — changing the SRC's `commit`/`ref` — which is a
  human decision, so CapyFun proposes it as a PR rather than acting on it. This
  is the gap.

This is exactly the per-rule trigger semantics already stated in
`automation.md`: "pinned → upstream pushes are ignored; a new release proposes a
**pin-bump PR** (edit the SRC's `ref`/`commit`). Merging that PR is itself the
source change that triggers the actual import/vendor."

## Flow

```
ReleaseEvent / tag CreateEvent ─► resolve latest release tag ─► compare to declared pin
        (a hint)                   (ls-remote + tag-candidate logic)        │
                                                                  newer? ───┤── no ─► no-op
                                                                            │
                                                                           yes
                                                                            ▼
                                                       open a PR editing the SRC's pin
                                                       (commit = "<new sha>")
                                                                            │
                                                              human reviews & merges
                                                                            ▼
                                                  the merge is a monorepo change that
                                                  triggers the actual vendor/import
```

A pin-bump PR **only edits config**. It does *not* vendor the new snapshot or
import new content — that happens on the next reconcile, after the merge. This
keeps the invariant intact: config declares edges; humans review edge changes;
the engine acts on the merged config.

## Resolving "is there a newer pin?"

The resolution machinery already exists in `vendorgen` and is reused verbatim:

- `ls_remote(slug)` lists the upstream's refs.
- `tag_candidates(name, version)` / `pick_commit(refs, candidates)` map a version
  to a tag to a commit, dereferencing annotated tags.

What's missing is the **current** side of the comparison. The SRC records a
`commit` (an opaque SHA) but not the tag it came from, so CapyFun can't say
"`v1.2.3` → `v1.3.0` available" without a reverse map. Two options:

1. **Record the resolved tag in the rule** (preferred). `gen-cargo`/`gen-npm`
   already know the tag (`Vendored.tag`); add an optional, informational
   `version`/`tag` attribute to `git_repository` (and the tag-pinned import). The
   pin's commit stays authoritative; the tag is the human-readable anchor for the
   comparison and the PR text. This keeps resolution deterministic and the PR
   diff legible (`commit`/`version` both bump).
2. **Compare commits only** (fallback). Without a recorded tag, report "pinned
   commit is not the latest release commit" — actionable but not legible. Useful
   as an interim before option 1 lands.

"Latest" for v0 is the newest release tag the upstream advertises; semver-range
constraints (only bump within `^1`, skip prereleases) are a later refinement, not
v0.

## Editing the SRC

The pin lives in a generated, canonical SRC line (`commit = "…"`, optionally
`version = "…"`). The bump rewrites those line(s):

- **v0:** a targeted, anchored rewrite of the `commit`/`version` string for the
  named rule (the SRC form is canonical and machine-generated, so this is
  tractable). Validate by re-evaluating the edited SRC and re-resolving the IR
  before opening the PR — never ship a config that fails `capyfun check`.
- **Later:** regenerate the package's SRC via the existing generator for that
  ecosystem, or a typed "set attribute" pass over the parsed rule, so the edit
  does not depend on textual shape.

## Relationship to `status`

`status` gains a pinned-freshness check for pinned targets: alongside the
existing `up to date` / `pin changed` (declared-vs-vendored), report
`pin bump <tag> available` when a newer upstream release exists than the declared
pin. This is the "pin bump vN available" line `automation.md` already promised
the `status` surface — currently unimplemented because of the missing current-tag
anchor above.

## Invariants (proposed)

- A pin bump is a **PR that edits config**, never a direct content change. The
  human merge is the trigger for the subsequent vendor/import. (Preserves "config
  declares edges; humans review edge changes.")
- Pinned targets **ignore push events**; only releases/tags propose bumps. (The
  server's existing `SubKind::Vendor` matching on `CreateEvent`/`ReleaseEvent`
  already encodes this; today it just no-ops.)
- Resolution reuses the deterministic `vendorgen` tag-candidate/`pick_commit`
  path; the proposed pin is reproducible from `(slug, tag)`.
- A proposed SRC edit must pass `capyfun check` (re-evaluate + validate) before
  the PR is opened; CapyFun never proposes config that won't compile.

## Milestones

- **P1 — Status freshness.** Record the resolved tag in `git_repository` /
  tag-pinned import (config + IR + the generators that already know it), and have
  `status` report `pin bump <tag> available` by resolving the latest tag and
  comparing. No PR yet — useful standalone.
- **P2 — Pin-bump PR.** On a `ReleaseEvent`/tag `CreateEvent` for a pinned
  target (or via `capyfun reconcile` for pinned targets), rewrite the SRC pin,
  re-validate, push a branch, and open a monorepo PR ("Bump acme/widget to
  v1.3.0"). Idempotent: an already-open bump PR for the same target/tag is
  refreshed, not duplicated.
- **P3 — Constraints.** Optional semver-range / prerelease-skip policy on which
  releases qualify.

## Open questions

- **Tag for an existing pin with no recorded version:** backfill by reverse-
  resolving the commit against `ls-remote` tags (best-effort) until P1's recorded
  tag is present everywhere.
- **Monorepo vs. non-monorepo upstreams:** `tag_candidates` already covers
  `pkg-vX`, `pkg@X`, etc.; confirm the chosen-latest logic across those shapes.
- **Interaction with per-target refs (`per-target-refs.md`):** a pin-bump PR is
  a config-edit PR, distinct from a content reconcile PR — confirm they use
  distinct refs/PRs and don't collide for the same target.
