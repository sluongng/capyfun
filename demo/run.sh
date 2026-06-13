#!/usr/bin/env bash
#
# Demo: turn a Go module's dependencies into CapyFun imports that carry real
# upstream Git history into the monorepo — a history-preserving alternative to
# `go mod vendor`.
#
# Requires network access to github.com. Runs in a throwaway temp directory so
# the repo's demo/ stays clean.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
echo "==> building capyfun"
cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
bin="$repo_root/target/debug/capyfun"

git_c() { git -c user.email=demo@example.com -c user.name=demo "$@"; }

work="$(mktemp -d)"
cp "$repo_root"/demo/{go.mod,go.sum,main.go,SRC} "$work"/
cd "$work"
git init -q -b main
git_c add -A >/dev/null
git_c commit -qm "demo app + root SRC" >/dev/null

echo
echo "==> capyfun gen-go  (read go.mod -> scaffold import SRC files)"
"$bin" gen-go --root "$work"
git_c add -A >/dev/null
git_c commit -qm "generate third_party import rules" >/dev/null

echo
echo "==> capyfun check  (validated IR for the generated tree)"
"$bin" check --root "$work" | head -16
echo "  ..."

for spec in google/uuid:uuid pkg/errors:errors spf13/pflag:pflag; do
    pkg="${spec%:*}"
    name="${spec#*:}"
    echo
    echo "==> capyfun import //third_party/github.com/$pkg:$name"
    "$bin" import "//third_party/github.com/$pkg:$name" --root "$work"
done

echo
echo "==> result: dependencies vendored WITH their upstream history"
echo "monorepo commits: $(git -C "$work" rev-list --count main)"
git -C "$work" log --oneline main | head -6
echo "  ..."
echo
echo "uuid source now lives in the tree:"
git -C "$work" ls-tree --name-only main third_party/github.com/google/uuid | sed 's/^/  /' | head -8

echo
echo "==> re-import is incremental + idempotent"
"$bin" import //third_party/github.com/google/uuid:uuid --root "$work"

echo
echo "Demo workspace: $work"
echo "Try: git -C $work log --oneline third_party/github.com/google/uuid"
