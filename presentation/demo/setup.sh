#!/usr/bin/env bash
#
# Build a throwaway, hermetic workspace for the asciinema demo:
#   - an upstream repo standing in for github.com/acme/widget (two commits)
#   - a monorepo whose third_party/widget/SRC declares a transform pipeline
#     (replace + move structural, apply_patch tip)
# Prints the monorepo path on the last line.
#
# Usage: setup.sh <workdir>
set -euo pipefail
WORK="${1:?usage: setup.sh <workdir>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
FIXTURE="$REPO_ROOT/examples/transforms/widget-origin"

gc(){ git -c user.email=demo@capyfun.dev -c user.name=CapyFun -c commit.gpgsign=false -C "$1" "${@:2}"; }

rm -rf "$WORK"; mkdir -p "$WORK"

# --- upstream (stands in for github.com/acme/widget) ------------------------
ORIGIN="$WORK/origins/acme/widget"
mkdir -p "$ORIGIN"
cp -r "$FIXTURE/." "$ORIGIN/"
gc "$ORIGIN" init -q -b main
gc "$ORIGIN" add -A
gc "$ORIGIN" commit -qm "initial widget"
sed -i 's#acme.internal/log v1.4.0#acme.internal/log v1.5.0#' "$ORIGIN/go.mod"
gc "$ORIGIN" commit -qam "bump acme.internal/log to v1.5.0"

# --- toolchain patch (the tip apply_patch transform) ------------------------
SCRATCH="$WORK/scratch"; mkdir -p "$SCRATCH"
gc "$SCRATCH" init -q
cp "$ORIGIN/go.mod" "$SCRATCH/go.mod"
gc "$SCRATCH" add -A; gc "$SCRATCH" commit -qm base
awk '{ print } /^go 1\.21$/ { print ""; print "toolchain go1.21.6" }' \
    "$ORIGIN/go.mod" > "$SCRATCH/go.mod.new"
mv "$SCRATCH/go.mod.new" "$SCRATCH/go.mod"

# --- monorepo with a transform pipeline -------------------------------------
MONO="$WORK/mono"
mkdir -p "$MONO/third_party/widget/patches"
gc "$MONO" init -q -b main
cat > "$MONO/SRC" <<'EOF'
monorepo(name = "tinytree", default_branch = "main")
EOF
cat > "$MONO/third_party/widget/SRC" <<'EOF'
# Import acme/widget, transforming it on the way in.
github_import(
    name = "widget",
    repo = "acme/widget",
    transforms = [
        # structural - rewrites every mirrored commit:
        replace(before = "acme.internal/", after = "", paths = ["**/*.go"]),
        move(src = "pkg", dst = "lib"),
        # tip - applied once on top of the mirror:
        apply_patch("patches/0001-pin-toolchain.patch"),
    ],
)
EOF
gc "$SCRATCH" diff > "$MONO/third_party/widget/patches/0001-pin-toolchain.patch"
gc "$MONO" add -A
gc "$MONO" commit -qm "configure widget import"

echo "$MONO"
