#!/usr/bin/env bash
#
# Demo: bring third-party dependencies for three ecosystems into the monorepo
# under third_party/{go,rust,js} — Go with full history (github_import), Rust and
# JS as pinned snapshots (git_repository). A history-preserving alternative to
# `go mod vendor` / `cargo vendor` / `node_modules`.
#
# Requires network access to github.com, crates.io, and registry.npmjs.org
# (the cargo/npm generators resolve repos + tags online). Runs in a throwaway
# temp directory.
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

echo
echo "==> capyfun gen-cargo  (Cargo.toml/Cargo.lock -> third_party/rust snapshots)"
"$bin" gen-cargo --root "$work"

echo
echo "==> capyfun gen-npm  (package.json/package-lock.json -> third_party/js snapshots)"
"$bin" gen-npm --root "$work"

git_c add -A >/dev/null
git_c commit -qm "regenerate import/snapshot rules" >/dev/null || true   # idempotent; may be a no-op

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
echo "==> Export: ship a monorepo-native path back out as a PR (the other direction)"
# Import/vendor bring code *in*; export ships a monorepo path *out* to a
# standalone repo as a PR. Use a local bare destination so the demo stays
# hermetic (no forge/network); PR creation is skipped and the gh command printed.
dests="$work/dests"; seed="$work/seed"; dest="$dests/acme/sdk-go"
mkdir -p "$seed"
git -C "$seed" init -q -b main
printf '# acme sdk-go\n' > "$seed/README.md"
git_c -C "$seed" add -A >/dev/null
git_c -C "$seed" commit -qm "initial" >/dev/null
git clone -q --bare "$seed" "$dest"
CAPYFUN_GITHUB_BASE="$dests" "$bin" export //sdk/go:go-sdk --no-pr --root "$work"
echo "exported tree on the destination (client/ prefix stripped, no SRC):"
git -C "$dest" ls-tree -r --name-only capyfun/export-go-sdk | sed 's/^/  /'
echo "scrubbed client.go (internal-only deleted, OSS-only uncommented):"
git -C "$dest" show capyfun/export-go-sdk:client.go | sed 's/^/  /'

echo
echo "==> result"
echo "monorepo commits: $(git -C "$work" rev-list --count main)"
echo "third_party tree:"
git -C "$work" ls-tree -r --name-only main third_party | grep -E '/(go.mod|Cargo.toml|package.json|LICENSE|license)$' | sed 's/^/  /' | head -12

echo
echo "Demo workspace: $work"
