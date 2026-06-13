# Demo: Go dependencies as history-preserving imports

This demo shows CapyFun as a **history-preserving alternative to `go mod
vendor`**. Instead of dropping opaque dependency snapshots into `vendor/`, it
imports each dependency's *real upstream Git history* into the monorepo under
`third_party/`, scaffolded automatically from `go.mod`.

## The pieces

- `go.mod` / `go.sum` / `main.go` — a tiny Go program depending on three small,
  dependency-free GitHub modules (`google/uuid`, `pkg/errors`, `spf13/pflag`).
- `SRC` — the root CapyFun config (`monorepo(...)`).
- `third_party/github.com/<owner>/<repo>/SRC` — **generated** import rules (one
  package per dependency), produced by `capyfun gen-go`.

## Run it

```sh
demo/run.sh        # needs network access to github.com
```

It performs the full flow in a throwaway temp directory:

1. **`capyfun gen-go --root .`** reads `go.mod`, maps each tag-pinned
   `github.com/...` dependency to a `github_import` rule, and writes a `SRC` file
   per dependency under `third_party/github.com/<owner>/<repo>/`. The tracked ref
   is the pinned version's tag (e.g. `refs/tags/v1.6.0`).
2. **`capyfun check --root .`** lowers the generated SRC tree to validated IR.
3. **`capyfun import //third_party/github.com/<owner>/<repo>:<repo>`** fetches the
   dependency and replays its first-parent history into the package — real
   commits, each tagged with `CapyFun-Origin` (and `CapyFun-Import`, which scopes
   the commit map so all three imports coexist on one branch).

The result is a monorepo whose `third_party/` holds the dependency source *with
hundreds of commits of real upstream history*, not a flat snapshot.

## Importing upstream changes later

When a dependency releases a new version:

1. bump it in `go.mod`,
2. re-run `capyfun gen-go` (rewrites the rule's `ref` to the new tag),
3. re-run `capyfun import` — only the **new commits** since the last import are
   replayed (incremental and idempotent).

To instead track a moving branch, set `ref = "refs/heads/main"` in the generated
`SRC`; each import then pulls whatever new commits landed upstream.

## Notes

- `go.sum` hashes here are illustrative; run `go mod tidy` for real ones. CapyFun
  reads `go.mod` for the module set/versions and uses `go.sum` only to cross-check
  presence.
- The generated `third_party/**/SRC` files are committed so you can see what
  `gen-go` produces; the imported source itself only lands in the temp workspace
  `run.sh` creates (it would be hundreds of MB of vendored history).
