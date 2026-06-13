#!/usr/bin/env bash
#
# Materialize //third_party/widget by running a real `capyfun import` against a
# local fixture built from ./widget-origin. Hermetic: no network, runs in a
# throwaway temp directory, rerunnable from a clean checkout.
#
# This uses the implemented plain-mirror `github_import` path (mirror + patch
# tip layer). The richer `transforms = [...]` pipeline in third_party/widget/SRC
# is the forward-looking T1-T5 preview and is NOT applied here.
#
# Usage: examples/transforms/materialize-widget.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

echo "==> building capyfun"
cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
bin="$repo_root/target/debug/capyfun"

git_c() { git -c user.email=widget@example.com -c user.name=widget -C "$1" "${@:2}"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- origin repository (stands in for github.com/acme/widget) ---------------
# Seeded from the checked-in widget-origin/ tree, with a second commit so the
# mirror has real first-parent history to preserve.
base="$work/origins"
origin="$base/acme/widget"
mkdir -p "$origin"
cp -r "$here/widget-origin/." "$origin/"
git_c "$origin" init -q -b main
git_c "$origin" add -A
git_c "$origin" commit -qm "initial widget"

# A follow-up upstream commit: bump the internal log dependency.
sed -i 's#acme.internal/log v1.4.0#acme.internal/log v1.5.0#' "$origin/go.mod"
git_c "$origin" commit -qam "bump acme.internal/log to v1.5.0"

# --- toolchain patch (the tip-layer local modification) ---------------------
# Generated from the origin's go.mod so it always applies cleanly.
scratch="$work/scratch"
mkdir -p "$scratch"
git_c "$scratch" init -q
cp "$origin/go.mod" "$scratch/go.mod"
git_c "$scratch" add -A
git_c "$scratch" commit -qm base
awk '{ print } /^go 1\.21$/ { print ""; print "toolchain go1.21.6" }' \
    "$origin/go.mod" > "$scratch/go.mod.new"
mv "$scratch/go.mod.new" "$scratch/go.mod"

# --- monorepo: runnable plain import config + the patch, committed on main ---
mono="$work/mono"
mkdir -p "$mono/third_party/widget/patches"
git_c "$mono" init -q -b main
cat > "$mono/SRC" <<'EOF'
monorepo(name = "tinytree", default_branch = "main")
EOF
cat > "$mono/third_party/widget/SRC" <<'EOF'
github_import(
    name = "widget",
    repo = "acme/widget",
    patches = ["patches/0001-pin-toolchain.patch"],
)
EOF
git_c "$scratch" diff > "$mono/third_party/widget/patches/0001-pin-toolchain.patch"
git_c "$mono" add -A
git_c "$mono" commit -qm "configure widget import"

# --- import -----------------------------------------------------------------
echo
echo "==> capyfun import //third_party/widget:widget"
CAPYFUN_GITHUB_BASE="$base" "$bin" import //third_party/widget:widget --root "$mono"

# --- show what landed -------------------------------------------------------
echo
echo "==> materialized tree (main:third_party/widget)"
git -C "$mono" ls-tree -r --name-only main third_party/widget | sed 's/^/  /'

echo
echo "==> third_party/widget/go.mod (upstream content + applied patch)"
git -C "$mono" show main:third_party/widget/go.mod | sed 's/^/  /'

echo
echo "==> history (mirror commits carry CapyFun-Origin; patch carries CapyFun-Patch)"
git -C "$mono" log main --format='  %h %s%n     %(trailers:key=CapyFun-Origin,key=CapyFun-Patch,separator=%x20)' \
  | sed '/^     *$/d'

echo
echo "PASS — widget materialized under third_party/widget/ with history preserved"
