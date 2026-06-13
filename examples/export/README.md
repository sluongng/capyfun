# Example: exporting a monorepo path out as a PR

This example is the **other half** of `examples/monorepo` (which imports code
*in*). Here a path developed *inside* the monorepo — a Go SDK at
`sdk/go/client/` — is published *out* to a standalone GitHub repo
(`acme/sdk-go`) as a pull request.

## Layout

```text
examples/export/
├── SRC                          # root: workspace anchor + monorepo()
└── sdk/
    └── go/
        ├── SRC                  # package //sdk/go: github_export(...)
        └── client/             # the exported source
            ├── client.go
            └── go.mod
```

## Import and export are asymmetric

CapyFun does not model sync as one symmetric pipe (see `../../CLAUDE.md`):

| | Import | Export |
|---|---|---|
| Direction | upstream mainline → monorepo path | monorepo path → destination repo |
| Granularity | per-commit mirror (history-preserving) | a reviewable changeset → one PR |
| Lands by | replaying onto the monorepo branch | opening a **PR** (never a direct push) |
| Commit map | `CapyFun-Origin: <sha>` trailer | `CapyFun-Export: <sha>` trailer |

## What an export produces

`capyfun export //sdk/go:go-sdk --root examples/export` does:

1. **Evaluate → IR → validate** the config (pure, no I/O), resolving the export
   edge `//sdk/go:go-sdk` (destination `acme/sdk-go`, branch `main`, source
   `sdk/go/client`).
2. **Fetch the destination branch** — the commit map's source of truth for what
   has already shipped (read from `CapyFun-Export` trailers).
3. **Project the `from` subtree onto the destination**: each new monorepo commit
   touching `sdk/go/client/` is replayed with the `client/` prefix **stripped**
   (so the SDK sits at the destination root), CapyFun's own `SRC` marker dropped,
   author/message preserved, and a `CapyFun-Export: <monorepo-sha>` trailer
   appended. Commits that do not change the exported content are skipped, so the
   result is a clean changeset.
4. **Push a branch** (`capyfun/export-go-sdk`) to the destination and **open a
   GitHub PR** against `main`. Export never pushes to the destination's default
   branch directly — a human or policy on the destination side reviews and
   merges.

On a re-export, the commit map makes it incremental and idempotent: with nothing
new it is a no-op; after a merge, only the delta ships.

## Run it (hermetic)

The end-to-end flow against a local stand-in destination (no network, no forge)
is in `scripts/smoke-export.sh`:

```sh
scripts/smoke-export.sh
```

It builds a local destination repo and a monorepo, runs `capyfun export`, and
asserts the export branch landed with the prefix stripped and the commit-map
trailer in place. PR creation is skipped for a local destination (the `gh pr
create` command is printed instead); against a real GitHub destination, CapyFun
shells out to the GitHub CLI to open the PR.
