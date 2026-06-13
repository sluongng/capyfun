#!/usr/bin/env bash
#
# Hermetic smoke test for `capyfun export`. Builds a local destination repository
# (a standalone "public SDK") and a monorepo in a fresh temp directory, runs an
# export through the real CLI binary (pointed at the local destination via
# CAPYFUN_GITHUB_BASE), and asserts the export branch landed with the `from`
# prefix stripped and a CapyFun-Export commit-map trailer. PR creation is skipped
# for a local destination, so no network or forge is needed. Rerunnable from a
# clean checkout.
#
# Usage: scripts/smoke-export.sh
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
echo "==> building capyfun"
cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
bin="$repo_root/target/debug/capyfun"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export GIT_AUTHOR_NAME=smoke GIT_AUTHOR_EMAIL=smoke@example.com
export GIT_COMMITTER_NAME=smoke GIT_COMMITTER_EMAIL=smoke@example.com

fail() { echo "FAIL: $*" >&2; exit 1; }

# --- destination repository (stands in for github.com/acme/sdk-go) ---
# libgit2's local push targets bare repos, so build a seed then clone it bare.
base="$work/dests"
seed="$work/seed"
dest="$base/acme/sdk-go"
mkdir -p "$seed"
git -C "$seed" init -q -b main
printf '# acme sdk-go\n' > "$seed/README.md"
git -C "$seed" add -A
git -C "$seed" commit -qm "initial"
git clone -q --bare "$seed" "$dest"

# --- monorepo with the SDK source under an export package, on main ---
mono="$work/mono"
mkdir -p "$mono/sdk/go/client"
git -C "$mono" init -q -b main
cat > "$mono/SRC" <<'EOF'
monorepo(name = "acme", default_branch = "main")
EOF
cat > "$mono/sdk/go/SRC" <<'EOF'
github_export(
    name = "go-sdk",
    repo = "acme/sdk-go",
    branch = "main",
    from_path = "client",
)
EOF
printf 'package client\n\nconst V = 1\n' > "$mono/sdk/go/client/client.go"
printf 'module acme/sdk-go\n' > "$mono/sdk/go/client/go.mod"
git -C "$mono" add -A
git -C "$mono" commit -qm "add go sdk client"

show() { git -C "$dest" show "$1:$2"; }
branch_tip() { git -C "$dest" rev-parse "$1"; }

echo "==> first export"
CAPYFUN_GITHUB_BASE="$base" "$bin" export //sdk/go:go-sdk --no-pr --root "$mono"

eb="capyfun/export-go-sdk"
git -C "$dest" rev-parse --verify "$eb" >/dev/null 2>&1 || fail "export branch not pushed"
show "$eb" client.go | grep -q "const V = 1"        || fail "exported content missing"
show "$eb" go.mod | grep -q "module acme/sdk-go"    || fail "exported go.mod missing"
git -C "$dest" show "$eb:SRC" >/dev/null 2>&1        && fail "CapyFun SRC must not be exported"
git -C "$dest" show "$eb:client" >/dev/null 2>&1     && fail "the from prefix must be stripped"
git -C "$dest" log "$eb" --format=%B | grep -q "CapyFun-Export" || fail "export trailer missing"

# --- simulate the PR being merged: fast-forward main to the export tip ---
git -C "$dest" update-ref refs/heads/main "$(branch_tip "$eb")"

echo "==> re-export (idempotent)"
out="$(CAPYFUN_GITHUB_BASE="$base" "$bin" export //sdk/go:go-sdk --no-pr --root "$mono")"
echo "$out" | grep -q "already up to date" || fail "re-export was not a no-op: $out"

echo "==> new monorepo change, delta export"
printf 'package client\n\nconst V = 2\n' > "$mono/sdk/go/client/client.go"
git -C "$mono" commit -qam "bump sdk client version"
CAPYFUN_GITHUB_BASE="$base" "$bin" export //sdk/go:go-sdk --no-pr --root "$mono"
show "$eb" client.go | grep -q "const V = 2" || fail "delta content missing"
[ "$(git -C "$dest" rev-parse "$eb^")" = "$(branch_tip main)" ] || fail "delta did not build on merged tip"

echo "PASS"
