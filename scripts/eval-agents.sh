#!/usr/bin/env bash
#
# CapyFun agent-transform eval harness.
#
# Runs each fixture under tests/fixtures/evals/ through the real `capyfun` import/
# export path with the hermetic `fixture` executor (a deterministic mock agent —
# no model, no network, zero cost) and prints a results table: success, verifier
# result, first-run time, replay (cache-hit) time, model calls, estimated cost.
#
# It doubles as a test: a fixture whose result does not match its declared
# EXPECT makes the script exit non-zero.
#
# Usage:
#   scripts/eval-agents.sh            # run all fixtures, print the table
#   scripts/eval-agents.sh api-migration dependency-modernize   # subset
#
# Enable the REAL agent path instead of the mock by configuring a logged-in
# harness CLI and running `capyfun import ... --executor local` directly; see
# docs/evals.md for the columns a real run reports.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=../tests/fixtures/evals/lib.sh
source "$HERE/../tests/fixtures/evals/lib.sh"

EVAL_DIR="$REPO_ROOT/tests/fixtures/evals"

eval_build_bin

# Which fixtures to run (CLI args, else all directories with a meta.env).
fixtures=()
if [ "$#" -gt 0 ]; then
	for n in "$@"; do fixtures+=("$EVAL_DIR/$n"); done
else
	for d in "$EVAL_DIR"/*/; do [ -f "$d/meta.env" ] && fixtures+=("${d%/}"); done
fi

rows=()       # markdown table rows
fail=0

run_import_fixture() {
	local f="$1" work base mono out1 out2 t0 t1 t2 verdict cache run1 replay misses
	work="$(mktemp -d)"; base="$work/origins"; mono="$work/mono"
	eval_setup_origin "$f" "$base" >/dev/null
	eval_setup_monorepo "$f" "$mono"

	t0=$(now_ms); out1="$(eval_import "$f" "$mono" "$base" 2>/dev/null)"; t1=$(now_ms)
	local verify; verify="$(eval_verify_final "$mono" "$DEST" "$VERIFY")"
	out2="$(eval_import "$f" "$mono" "$base" 2>/dev/null)"; t2=$(now_ms)

	run1=$((t1 - t0)); replay=$((t2 - t1))
	misses="$(eval_misses "$out1")"
	local hits2; hits2="$(eval_hits "$out2")"
	cache="miss → hit"
	[ "${hits2:-0}" -ge 1 ] || cache="miss → MISS(?)"

	if [ "$verify" = "$EXPECT" ]; then verdict="✅ $verify"; else verdict="❌ $verify (want $EXPECT)"; fail=1; fi

	# The mock makes no real model calls; the "calls" column is the cache-miss
	# count (= how many model calls the REAL path would make on a cold cache).
	rows+=("| $(basename "$f") | $AGENT_DESC | $verdict | \`$VERIFY\` | ${run1} | ${replay} | $cache | ${misses:-0} (mock) | \$0.00 (mock) |")
	rm -rf "$work"
}

run_export_fixture() {
	local f="$1" work base mono dest branch t0 t1 verify scrub verdict run1
	work="$(mktemp -d)"; base="$work/origins"; mono="$work/mono"
	eval_setup_monorepo "$f" "$mono"
	t0=$(now_ms); dest="$(eval_export "$f" "$mono" "$base")"; t1=$(now_ms)
	branch="capyfun/export-${LABEL##*:}"
	run1=$((t1 - t0))

	# Verify the exported tree: builds clean AND no internal markers leaked.
	local out; out="$(mktemp -d)"
	git -C "$dest" archive "$branch" 2>/dev/null | tar -x -C "$out" 2>/dev/null || true
	if (cd "$out" && eval "$VERIFY") >/dev/null 2>&1; then verify="pass"; else verify="fail"; fi
	if [ -n "${SCRUB_FORBID:-}" ] && git -C "$dest" grep -nE "$SCRUB_FORBID" "$branch" -- '*.go' >/dev/null 2>&1; then
		scrub="LEAK"
	else
		scrub="scrubbed"
	fi
	rm -rf "$out"

	if [ "$verify" = "$EXPECT" ] && [ "$scrub" = "scrubbed" ]; then verdict="✅ $verify, $scrub"; else verdict="❌ $verify, $scrub"; fail=1; fi
	rows+=("| $(basename "$f") | $AGENT_DESC | $verdict | \`$VERIFY\` | ${run1} | — | n/a (no agent) | 0 | \$0.00 |")
	rm -rf "$work"
}

for f in "${fixtures[@]}"; do
	[ -f "$f/meta.env" ] || { echo "no such fixture: $f" >&2; exit 2; }
	eval_load_meta "$f"
	echo "==> eval: $(basename "$f") — $DESC" >&2
	case "$KIND" in
		import) run_import_fixture "$f" ;;
		export) run_export_fixture "$f" ;;
		*) echo "unknown KIND=$KIND in $f/meta.env" >&2; exit 2 ;;
	esac
done

echo
echo "## CapyFun eval results (hermetic, fixture/mock agents)"
echo
echo "| Fixture | Agent (harness + model) | Result | Verifier | Run 1 (ms) | Replay (ms) | Cache | Model calls | Est. cost |"
echo "|---------|-------------------------|--------|----------|-----------:|------------:|-------|------------:|-----------|"
for r in "${rows[@]}"; do echo "$r"; done
echo
echo "_Mock agents run deterministically with no model access. \"Model calls\" is the"
echo "cold-cache miss count — the number of model calls the **real** \`--executor local\`"
echo "path would make; every replay is a content-addressed cache hit and is free._"

if [ "$fail" -ne 0 ]; then
	echo >&2
	echo "FAIL: at least one fixture did not match its expected result" >&2
	exit 1
fi
echo
echo "All fixtures passed."
