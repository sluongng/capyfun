# CapyFun Automation: event-driven import/export

This document specifies how CapyFun reacts to upstream and monorepo changes —
listening for GitHub activity and (re)running imports, vendors, and exports on the
affected targets. It builds on the import/vendor/export engine and the commit map.

Read `../../CLAUDE.md` first. This is a design/roadmap artifact; the buildable
first slice (the poll reconciler) is spec'd concretely at the end.

## Thesis: level-triggered, events are hints

CapyFun does **not** map an event directly to an action. An event wakes a
**reconciler** that compares, per target, *desired upstream state* against
*actual state* (the commit map) and acts on the diff. This is the
Kubernetes-controller model.

Why: every event channel is lossy, delayed, or reorderable. If each event drove a
specific action, CapyFun would drift. Because config evaluation is pure and
import/vendor are idempotent (via the `CapyFun-Origin` / `CapyFun-Vendor`
trailers), the reconciler can always recompute the desired state and converge. A
missed or late event just means slightly higher latency until the next hint or
the periodic backstop sweep. **Never depend on an event being delivered.**

## Event sources (three ingress paths, one reconciler)

```
GH Archive firehose  → public third-party upstreams (hourly, lossy-ok)   ┐
GitHub App webhooks  → owned/installed repos (private OK, low latency)    ├─► reconciler
ls-remote poll       → backstop floor for everything                     ┘   (level-triggered)
```

| Source | Covers | Latency | Notes |
|---|---|---|---|
| **GH Archive** | all **public** repos | ~hourly | the primary signal for third-party imports |
| **GitHub App** | repos where the app is **installed** (owned) | seconds | also the identity used to *act* |
| **ls-remote poll** | literally everything | configurable | correctness backstop; never fully retired |

A GitHub App only delivers webhooks for repos it is installed on, which you can
only install on repos you admin — so it cannot see the long tail of third-party
upstream dependencies. That tail is what GH Archive and polling are for.

## Primary source: GH Archive

[GH Archive](https://www.gharchive.org/) records the entire public GitHub event
stream as hourly `*.json.gz` batches (also on BigQuery). CapyFun consumes the
latest archive each hour and keeps only events whose repo appears in the global
dependency index (below). Relevant event types:

- **`PushEvent`** — `ref` + head SHA → reconcile importers tracking that ref.
- **`CreateEvent`** (`ref_type=tag`) / **`ReleaseEvent`** → a new release →
  propose a pin bump for tag/commit-pinned imports and vendors.

The value of GH Archive is **selectivity**: instead of `ls-remote`-ing thousands
of tracked upstreams every cycle (`O(all deps)`), CapyFun only reconciles targets
whose upstream actually emitted an event this hour (`O(changed deps)`). The
tradeoffs — **public-only**, **~1h latency**, **lossy** — are all acceptable
because the reconciler is level-triggered and the poll backstop covers the gaps
(private deps, filtered/missed events). Start with the hourly batch; a real-time
consumer (Events API) is a later latency optimization.

## The load-bearing structure: a global reverse index

Filtering the firehose needs `repo → [affected targets across all monorepos]`.
This is just the aggregation of every monorepo's IR — each `github_import` /
`git_repository` already declares its `repo` (and `ref`/`commit`). One consumer
reads the firehose, looks up each event's repo, and fans out reconcile jobs. It is
a hot path; it must be fast and is sized by total dependency edges, not by repo
count.

## The reconciler

For each affected target:

1. **Desired**: resolve the upstream state — `ls-remote` the tracked `ref` (for
   history-tracking `github_import`), or the declared `commit`/tag (for pinned
   `git_repository` and tag-pinned imports).
2. **Actual**: read the commit map (trailer scan scoped by `CapyFun-Import: <dest>`).
3. **Act on the diff**: import the new first-parent delta / vendor the new
   snapshot / open a pin-bump PR / refresh an export PR. No diff → no-op.

Re-runs are safe (idempotent), so jobs can be retried and events can be
at-least-once. Two CLI surfaces fall out:

- **`capyfun status`** — dry-run reconcile: per target, "up to date / N commits
  behind / pin bump vN available." No infra, useful standalone.
- **`capyfun reconcile [//target]`** — perform the import/vendor/export and emit
  the output below.

## Per-rule trigger semantics

- `github_import` tracking `refs/heads/main` → import the delta on `PushEvent`.
- `github_import` / `git_repository` **pinned** → upstream pushes are ignored; a
  new release proposes a **pin-bump PR** (edit the SRC's `ref`/`commit`). Merging
  that PR is itself the source change that triggers the actual import/vendor.
- `github_export` → a monorepo-side change in `from_path` (or a merged monorepo
  PR) → push branch + open/refresh the destination PR.

## Output: one PR (and branch) per target

Each reconcile writes to its **own** ref (`refs/capyfun/<label>`) and opens its
own monorepo PR ("Import acme/backend: 5 new commits"), with status reported via
the Checks API. Benefits:

- per-dependency review that respects the monorepo's review process;
- no contention between concurrent reconciles;
- it **resolves the buried-patch-layer limitation** noted in
  `transformations.md` (re-importing a dep whose commits sit under another) —
  per-target branches keep each import's mirror+patch layer self-contained.

Trusted read-only mirrors may auto-commit instead of opening a PR (policy).

## The GitHub App's real role

Register the App for two reasons that have nothing to do with the firehose:

1. **Identity to act** — installation tokens to push branches, open the monorepo
   and export PRs, and post Check runs. Needed no matter how events arrive.
2. **Low-latency webhooks on owned repos** — exports (you own the destination)
   and imports from your own forks/mirrors, including private ones.

## Safety

Imports run **arbitrary upstream content** through transforms, and
`agent_transform` literally executes a coding agent over it. So:

- run reconciles in a **sandbox** with least-privilege tokens and a network
  policy (especially for generative transforms);
- verify webhook HMAC signatures; dedupe events by `(repo, ref, sha)`;
- **default to PR-for-review, not auto-merge**, for anything that runs transforms;
- debounce/batch bursts of pushes per target.

## Architecture

```
GH Archive (hourly)  ─┐
GitHub App webhooks  ─┼─► [ ingest ]  verify + dedupe + repo→targets via global index
ls-remote sweep ─────┘        │
                              ▼
                          [ queue ]   one job per (target, desired-state)
                              │
                              ▼
                          [ runner ]  capyfun reconcile: desired-vs-commit-map, import/vendor/export
                              │
                              ▼
                          [ deliver ] per-target PR branch + Checks status
```

Lightweight ingest (a Cloudflare Worker fits TinyTree's stack) + a heavy executor
(container/VM or GitHub Actions running the `capyfun` binary with git + fetch +
any agent sandboxes).

## Phasing

1. **Poll reconciler** — `capyfun status` then `capyfun reconcile`, driven by
   cron. Works for every repo (public/private/owned/not), zero GitHub
   integration. A complete product on its own.
2. **GitHub App** — for auth (PRs, Checks, tokens) and low-latency webhooks on
   owned repos.
3. **GH Archive consumer** — hourly firehose → global index → reconcile, as the
   latency/scale optimization for public third-party deps.

Because it is level-triggered, (1) ships alone; (2) and (3) only reduce latency.

## Open questions

- Index storage and freshness: how do per-monorepo IRs aggregate into the global
  `repo → targets` index, and how fast must it be on the firehose hot path?
- Pin-bump policy: auto-open bump PRs for every release, or only for
  semver-allowed ranges declared in config?
- Private third-party deps: firehose-invisible and not installable → polling is
  the permanent floor; what default cadence?
- GH Archive ingestion: hourly batch (cheap MVP) vs a real-time Events API
  consumer (lower latency, more infra) — when is the switch worth it?
- Per-target PR churn: how to batch many small upstream updates without flooding
  reviewers (digest PRs? scheduled windows?).
