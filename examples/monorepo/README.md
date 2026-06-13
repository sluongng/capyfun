# Example: a CapyFun mini-monorepo

This is a small but realistic monorepo layout showing CapyFun's config model:
distributed `SRC` files (Bazel packages), `.scl` libraries loaded via `load()`,
a GitHub import via a macro, and a local patch series.

## Layout

```text
examples/monorepo/
├── SRC                                  # root: workspace anchor + monorepo()
├── lib/
│   └── github.scl                      # library: `vendored` macro (like .bzl)
└── third_party/
    └── backend/
        ├── SRC                          # package //third_party/backend
        └── patches/
            ├── 0001-pin-go-toolchain.patch
            └── 0002-drop-internal-telemetry.patch
```

## Config model (mirrors Bazel's BUILD / .bzl split)

- **SRC files** *instantiate* source rules. Like `BUILD`. The file named `SRC`.
- **`.scl` libraries** define reusable macros and constants, loaded via
  `load("//path/to/lib.scl", "symbol")`. Like `.bzl`. They never instantiate
  rules themselves (a top-level builtin call in a library is an error).
- **Builtins are the only rules**: `monorepo`, `github_import`, `github_export`.
  Macros are pure composition sugar — they expand to builtin calls and add no
  projection logic, so the normalized IR is exactly what hand-written builtin
  calls would produce. (Custom `rule()` definitions are out of scope for now.)

### Distributed SRC files (packages)

CapyFun discovers every file named `SRC` under the monorepo root. Each one is a
**package** at its directory:

- The **root SRC** declares the `monorepo` singleton (exactly once) and anchors
  `//`-rooted load paths.
- A **sub-package SRC** declares the import/export edges for its directory. The
  import lands in the package that declares it — `into` defaults to the SRC's
  own package directory, so `//third_party/backend`'s import mirrors upstream
  under `third_party/backend/`. (Destinations can't overlap by construction.)
- Rules are referenced by label: `//third_party/backend:backend`.

## What an import produces (target behavior)

Import builds the destination path in two layers so faithfulness and local
modifications stay separable:

1. **Mirror layer** — each upstream commit along the **first-parent** chain of
   `ref` is replayed into the package directory, tree spliced under the prefix,
   author/message preserved, with a `CapyFun-Origin: <sha>` trailer. Merges are
   linearized to their first parent, so the monorepo gets one commit per
   first-parent step. This layer is a pristine, per-commit reflection of
   upstream's mainline.
2. **Patch layer** — after the mirror reaches the new upstream tip, the declared
   patch series is applied as CapyFun-authored commits on top, each carrying a
   `CapyFun-Patch: <file>` trailer. Patch paths are relative to the SRC file and
   the diffs are upstream-shaped (could be sent upstream unchanged).

On an incremental re-import, new upstream commits extend the mirror layer and
the patch layer is re-applied (rebased) onto the new tip. A patch that fails to
apply aborts the import with a clear diagnostic; nothing is half-written.

This keeps every imported file explainable back to its origin commit (mirror
layer) while still letting the monorepo build with required local changes (patch
layer), and it never silently diverges from upstream.
