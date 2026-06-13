# CapyFun Import Roadmap

This roadmap drives the first phase of CapyFun: a working **import** round-trip
that replays an external repository's commits into a TinyTree monorepo path,
incrementally and deterministically. Export is deferred until import is solid.

Read `../../CLAUDE.md` first for the project thesis. Keep each milestone small,
runnable, and tested. Run `cargo test` and `cargo clippy` before finishing one.

## Key decisions

- **Language:** Rust. Git plumbing via `git2` (libgit2) for the first prototype;
  revisit `gitoxide` later.
- **Config:** own narrow Starlark dialect via the `starlark` crate. Simpler than
  Copybara — declare edges, not transforms. GitHub-first builtins: `monorepo`,
  `github_import`, `github_export`.
- **Config files (Bazel-shaped):** rules are instantiated in `SRC` files (the
  `BUILD` analogue); `.star` libraries (the `.bzl` analogue) define macros +
  constants and are pulled in with `load("//path/to/lib.star", "sym")`.
  Libraries never instantiate rules. Macros expand to builtins only (no custom
  `rule()` yet).
- **Distributed packages:** every `SRC` file under the monorepo root is a
  package. The root SRC holds the single `monorepo(...)` and anchors `//` load
  paths. An import's `into` defaults to its package directory (optional subpath
  within the package), so destinations can't overlap. Rules are referenced by
  Bazel-style label `//pkg:name`; names are unique per package.
- **Commit map source of truth:** a `CapyFun-Origin: <origin-sha>` trailer on
  each mirrored commit (and `CapyFun-Patch: <file>` on patch-layer commits).
  Durable, survives clone, greppable, and doubles as the `explain` mechanism. A
  `refs/notes/capyfun` notes ref may mirror it later for O(1) lookup, but the
  trailer is canonical.
- **Import semantics:** per-commit along the **first-parent** chain of the
  origin ref, history-preserving, no squashing. Merges are linearized to their
  first parent.
- **Patches:** an optional, ordered series of static patch files rides in a
  separate **patch layer** on top of the pristine mirror — never baked into
  mirror commits. Re-import rebases the patch layer onto the new mirror tip.

## Import mechanic (target behavior)

1. Fetch the origin ref (e.g. `refs/heads/main`) into a local object store.
2. Determine the last-imported origin commit from the commit map (scan
   `CapyFun-Origin` trailers on the monorepo branch, or the notes ref later).
3. Walk new origin commits along the **first-parent** chain (oldest → newest).
4. For each origin commit, **splice** its tree under the `into` prefix into the
   current monorepo tree, producing a new monorepo tree OID.
5. Create a mirror commit: parent = monorepo HEAD, tree = spliced tree,
   author/committer/message preserved, plus the `CapyFun-Origin` trailer.
6. Advance HEAD and repeat. Re-running with no new origin commits is a no-op.
7. If the import declares `patches`, drop any existing patch-layer commits, then
   apply the series on top of the new mirror tip as `CapyFun-Patch` commits. A
   patch that fails to apply aborts the whole import.

## Milestones

### M0 — Rust project skeleton + CLI stubs

- `Cargo.toml`, crate layout: `config`, `ir`, `validate`, `engine`, `cli`.
- `clap`-based `capyfun` binary with stub `import` / `export` subcommands.
- Acceptance: `cargo build`, `cargo test`, `cargo clippy` all clean.

### M1 — Starlark config evaluator (SRC + libraries)

- `starlark` crate with typed builtins `monorepo(...)`, `github_import(...)`,
  `github_export(...)` captured into in-memory declarations, attributed to the
  declaring SRC package. Pure, no I/O. `github_import` fields: `name`,
  `repo` ("owner/name"), `ref`, optional `into` (subpath within the package),
  optional `patches` (list of paths relative to the SRC file).
- `load("//path/to/lib.star", "sym")` resolves `//` to the monorepo root (the
  dir holding the root SRC). Libraries may define macros/constants but a
  top-level builtin call in a library errors.
- Discover every `SRC` file under a root; evaluate each as a package.
- Acceptance: the `examples/monorepo` tree evaluates; the `vendored` macro
  expands to a `github_import`; an invalid builtin call (missing/extra field,
  bad `repo` slug) and a top-level builtin call in a library both error.

### M2 — Normalized IR + static validation

- Deterministic, serializable IR (serde). Convert captured decls → IR with
  resolved labels and package-anchored destination paths.
- Validate: `into` subpath is normalized relative (no `..`, not absolute) and
  stays within its package; destinations don't overlap; exactly one
  `monorepo(...)` (in the root SRC); names unique per package; labels resolve;
  non-empty `repo`/`ref`; `repo` is a well-formed `owner/name` slug; patch paths
  are normalized relative.
- Acceptance: snapshot test for the `examples/monorepo` IR; a test per rule.

### M3 — Tree-prefix rewrite core (JOSH-shaped)

- `engine`: given an origin tree OID and an `into` prefix, splice it into a base
  monorepo tree, returning a new tree OID. Deterministic and content-addressed.
- Acceptance: unit tests on bare-repo fixtures — prefixing an empty base, an
  existing base, nested prefixes; identical inputs yield identical OIDs.

### M4 — Single-commit import (mirror layer)

- Replay one origin commit into the monorepo path using the M3 primitive. New
  mirror commit preserves author/committer/message and appends the
  `CapyFun-Origin` trailer. Parent is the current monorepo HEAD.
- Acceptance: importing one commit produces the expected tree, trailer, and
  metadata; the trailer round-trips through parse.

### M5 — Incremental multi-commit import (mirror layer)

- Read the commit map (trailer scan), find the last-imported origin commit, walk
  the new first-parent range, replay as N linear mirror commits.
- Acceptance: importing a 3-commit origin yields 3 mirror commits in order;
  re-running imports nothing (idempotent); importing after new origin commits
  imports only the delta; a merge on the origin is linearized to first-parent.

### M6 — Patch layer (rebased on the mirror tip)

- After the mirror reaches the new tip, apply the declared `patches` series as
  `CapyFun-Patch` commits on top. On re-import, drop existing patch-layer
  commits and re-apply onto the new mirror tip. A failing patch aborts the
  import with a clear diagnostic; nothing is half-written.
- Acceptance: with a 2-patch series, import yields mirror commits + 2 patch
  commits; the worktree reflects the patches; re-import after new upstream
  commits keeps exactly 2 patch commits at the tip; a non-applying patch fails
  cleanly and leaves the monorepo branch unchanged.

### M7 — End-to-end import CLI + smoke test

- `capyfun import //pkg:name --root <monorepo>` wired through discover SRC →
  evaluate → IR → validate → engine, covering both mirror and patch layers.
- `scripts/smoke-import.sh` builds local bare repos from scratch, runs an
  import, and asserts the monorepo result. Rerunnable from a clean temp dir.
- Acceptance: smoke script passes from a clean checkout.

### M8+ — Export (deferred)

- Strip the `into` prefix from monorepo-side changes, push a branch to the
  destination remote, open a GitHub PR (`octocrab` or `gh` shell-out), using the
  commit map to know what has already shipped. Designed only after import lands.

## Roadmap rules

- Do not build export before import is solid.
- Keep config evaluation pure; all Git/network I/O lives in `engine`.
- Prefer hermetic bare-repo fixtures over network calls in tests.
- Update this doc when the sequence or decisions change.
