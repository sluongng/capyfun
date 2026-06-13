#!/usr/bin/env bash
#
# Demo: bring third-party dependencies for three ecosystems into the monorepo
# under third_party/{go,rust,js} — Go with full history (github_import), Rust and
# JS as pinned snapshots (git_repository). A history-preserving alternative to
# `go mod vendor` / `cargo vendor` / `node_modules`.
#
# Requires network access to github.com. Runs in a throwaway temp directory.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
echo "==> building capyfun"
cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
bin="$repo_root/target/debug/capyfun"

git_c() { git -c user.email=demo@example.com -c user.name=demo "$@"; }

work="$(mktemp -d)"
cp -r "$repo_root"/demo/. "$work"/
cd "$work"
git init -q -b main
git_c add -A >/dev/null
git_c commit -qm "demo manifests + config" >/dev/null

echo
echo "==> capyfun gen-go  (go.mod -> third_party/go import rules)"
"$bin" gen-go --root "$work" --prefix third_party/go
git_c add -A >/dev/null
git_c commit -qm "regenerate go imports" >/dev/null || true   # idempotent; may be a no-op

echo
echo "==> capyfun check  (whole tree -> validated IR)"
"$bin" check --root "$work" | grep -E '"label"' | sed 's/^/  /'

echo
echo "==> Go: import with full upstream history"
for spec in google/uuid:uuid pkg/errors:errors spf13/pflag:pflag; do
    "$bin" import "//third_party/go/github.com/${spec%:*}:${spec#*:}" --root "$work"
done

echo
echo "==> Rust: vendor pinned snapshots"
"$bin" vendor //third_party/rust/anyhow:anyhow --root "$work"
"$bin" vendor //third_party/rust/thiserror:thiserror --root "$work"

echo
echo "==> JS: vendor pinned snapshots (one big SRC, two targets)"
"$bin" vendor //third_party/js:escape-string-regexp --root "$work"
"$bin" vendor //third_party/js:ansi-styles --root "$work"

echo
echo "==> result"
echo "monorepo commits: $(git -C "$work" rev-list --count main)"
echo "third_party tree:"
git -C "$work" ls-tree -r --name-only main third_party | grep -E '/(go.mod|Cargo.toml|package.json|LICENSE|license)$' | sed 's/^/  /' | head -12

echo
echo "Demo workspace: $work"
