#!/usr/bin/env bash
#
# Hermetic smoke test for `capyfun import`. Builds a local origin repository and
# a monorepo in a fresh temp directory, runs an import through the real CLI
# binary (pointed at the local origin via CAPYFUN_GITHUB_BASE), and asserts the
# mirror + patch layers landed. Rerunnable from a clean checkout.
#
# Usage: scripts/smoke-import.sh
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

# --- origin repository (stands in for github.com/acme/backend) ---
base="$work/origins"
origin="$base/acme/backend"
mkdir -p "$origin"
git -C "$origin" init -q -b main
cat > "$origin/go.mod" <<'EOF'
module acme/backend

go 1.21

// pad a
// pad b
// pad c
// pad d

require cobra v1.8.0
EOF
printf 'package main\n' > "$origin/main.go"
git -C "$origin" add -A
git -C "$origin" commit -qm "init"

# --- generate a patch (add a toolchain line) via a scratch repo ---
scratch="$work/scratch"
mkdir -p "$scratch"
git -C "$scratch" init -q
cp "$origin/go.mod" "$scratch/go.mod"
git -C "$scratch" add -A
git -C "$scratch" commit -qm v1
awk '{ print } /^go 1\.21$/ { print "toolchain go1.21.6" }' "$origin/go.mod" > "$scratch/go.mod.new"
mv "$scratch/go.mod.new" "$scratch/go.mod"

# --- monorepo with config + the patch, committed on main ---
mono="$work/mono"
mkdir -p "$mono/third_party/backend/patches"
git -C "$mono" init -q -b main
cat > "$mono/SRC" <<'EOF'
monorepo(name = "acme", default_branch = "main")
EOF
cat > "$mono/third_party/backend/SRC" <<'EOF'
github_import(
    name = "backend",
    repo = "acme/backend",
    patches = ["patches/0001-toolchain.patch"],
)
EOF
git -C "$scratch" diff > "$mono/third_party/backend/patches/0001-toolchain.patch"
git -C "$mono" add -A
git -C "$mono" commit -qm "configure import"

show() { git -C "$mono" show "main:$1"; }
log_bodies() { git -C "$mono" log main --format=%B; }

echo "==> first import"
CAPYFUN_GITHUB_BASE="$base" "$bin" import //third_party/backend:backend --root "$mono"

show third_party/backend/go.mod | grep -q "require cobra v1.8.0" || fail "upstream content missing"
show third_party/backend/go.mod | grep -q "toolchain go1.21.6"   || fail "patch not applied"
show third_party/backend/main.go | grep -q "package main"        || fail "main.go missing"
show third_party/backend/SRC | grep -q "github_import"           || fail "SRC metadata lost"
show third_party/backend/patches/0001-toolchain.patch | grep -q "diff --git" || fail "patch metadata lost"
log_bodies | grep -q "CapyFun-Origin"                            || fail "mirror trailer missing"
log_bodies | grep -q "CapyFun-Patch"                             || fail "patch trailer missing"

tip1="$(git -C "$mono" rev-parse main)"

echo "==> re-import (idempotent)"
CAPYFUN_GITHUB_BASE="$base" "$bin" import //third_party/backend:backend --root "$mono"
[ "$(git -C "$mono" rev-parse main)" = "$tip1" ] || fail "re-import was not a no-op"

echo "==> new upstream commit, delta import"
sed -i 's/require cobra v1.8.0/require cobra v1.9.0/' "$origin/go.mod"
git -C "$origin" commit -qam "bump cobra"
CAPYFUN_GITHUB_BASE="$base" "$bin" import //third_party/backend:backend --root "$mono"
[ "$(git -C "$mono" rev-parse main)" != "$tip1" ] || fail "delta did not advance main"
show third_party/backend/go.mod | grep -q "require cobra v1.9.0" || fail "delta content missing"
show third_party/backend/go.mod | grep -q "toolchain go1.21.6"   || fail "patch not rebased"

echo "PASS"
