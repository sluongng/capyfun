#!/usr/bin/env bash
#
# Shared helpers for the CapyFun eval/demo harness. Sourced by
# scripts/eval-agents.sh and demo/full-loop.sh.
#
# Everything here is hermetic: it builds local Git repos in a temp directory and
# drives `capyfun` with the `fixture` executor (a deterministic mock agent — no
# model, no network, zero cost). See tests/fixtures/evals/README.md.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BIN="${CAPYFUN_BIN:-$REPO_ROOT/target/debug/capyfun}"

eval_build_bin() {
	echo "==> building capyfun" >&2
	cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml"
}

# Git with a fixed identity, scoped to a repo dir: eval_git <dir> <args...>
eval_git() { git -c user.email=eval@capyfun -c user.name=eval -C "$1" "${@:2}"; }

# Millisecond wall clock (GNU date).
now_ms() { date +%s%3N; }

# Load a fixture's meta.env into the current shell. Clears prior fields first.
eval_load_meta() {
	local dir="$1"
	KIND= DESC= LABEL= REPO= DEST= AGENT= AGENT_DESC= VERIFY= RETRIES=1 EXPECT=pass SCRUB_FORBID=
	# shellcheck disable=SC1090
	source "$dir/meta.env"
}

# Build origins/<REPO> from the fixture's commits/* directories, one commit per
# directory in sorted order (each overlays the working tree). Stands in for the
# upstream GitHub repo; reached via CAPYFUN_GITHUB_BASE.
eval_setup_origin() {
	local fixture="$1" base="$2"
	local origin="$base/$REPO"
	mkdir -p "$origin"
	eval_git "$origin" init -q -b main
	local c
	for c in "$fixture"/commits/*/; do
		cp -r "$c". "$origin"/
		eval_git "$origin" add -A
		eval_git "$origin" commit -qm "upstream $(basename "$c")"
	done
	echo "$origin"
}

# Copy a fixture's monorepo/ into a fresh git repo and commit it.
eval_setup_monorepo() {
	local fixture="$1" mono="$2"
	mkdir -p "$mono"
	cp -r "$fixture"/monorepo/. "$mono"/
	eval_git "$mono" init -q -b main
	eval_git "$mono" add -A
	eval_git "$mono" commit -qm "configure $(basename "$fixture")"
}

# Overlay an extra subtree from another fixture's monorepo/ into an existing
# monorepo (no re-init), then commit it. Used by the demo to add an export edge
# alongside an import edge in one monorepo. `subpath` is relative to monorepo/.
eval_overlay_monorepo() {
	local fixture="$1" mono="$2" subpath="$3"
	mkdir -p "$mono/$(dirname "$subpath")"
	cp -r "$fixture/monorepo/$subpath" "$mono/$(dirname "$subpath")/"
	eval_git "$mono" add -A
	eval_git "$mono" commit -qm "add $subpath from $(basename "$fixture")"
}

# Run `capyfun import` with the fixture (mock) executor. Prints the one-line
# import summary on stdout (callers parse `cache Xh/Ym` from it).
#   eval_import <fixture> <mono> <base> [--refresh]
eval_import() {
	local fixture="$1" mono="$2" base="$3"; shift 3
	CAPYFUN_GITHUB_BASE="$base" \
	CAPYFUN_AGENT_FIXTURE="$fixture/agent" \
	CAPYFUN_VERIFY="$VERIFY" \
	CAPYFUN_VERIFY_RETRIES="${RETRIES:-1}" \
		"$BIN" import "$LABEL" --root "$mono" --executor fixture "$@"
}

# Count cache hits / misses from an import summary line ("... cache 0h/1m").
eval_hits() { sed -nE 's/.*cache ([0-9]+)h\/([0-9]+)m.*/\1/p' <<<"$1"; }
eval_misses() { sed -nE 's/.*cache ([0-9]+)h\/([0-9]+)m.*/\2/p' <<<"$1"; }

# Authoritative verifier: check out DEST from the monorepo HEAD and run VERIFY
# there. This is independent of the in-loop agent verifier, so it reports an
# honest pass/fail even on a cache-hit replay (where the agent never runs).
# Echoes "pass" or "fail".
eval_verify_final() {
	local mono="$1" dest="$2" verify="$3"
	local out; out="$(mktemp -d)"
	if ! git -C "$mono" archive main:"$dest" 2>/dev/null | tar -x -C "$out" 2>/dev/null; then
		rm -rf "$out"; echo "fail"; return
	fi
	if (cd "$out" && eval "$verify") >/dev/null 2>&1; then
		echo "pass"
	else
		echo "fail"
	fi
	rm -rf "$out"
}

# Run an export fixture against a local bare destination (hermetic, --no-pr).
# Echoes the path to the destination bare repo.
eval_export() {
	local fixture="$1" mono="$2" base="$3"
	local dest="$base/$REPO" seed; seed="$(mktemp -d)"
	eval_git "$seed" init -q -b main
	printf '# %s\n' "$REPO" > "$seed/README.md"
	eval_git "$seed" add -A
	eval_git "$seed" commit -qm initial
	mkdir -p "$dest"
	git clone -q --bare "$seed" "$dest"
	rm -rf "$seed"
	CAPYFUN_GITHUB_BASE="$base" "$BIN" export "$LABEL" --no-pr --root "$mono" >/dev/null
	echo "$dest"
}
