#!/usr/bin/env bash
#
# CapyFun full-loop demo — the whole story in one hermetic command.
#
#   upstream change → import → agent transform → verifier → export → replay
#
# It is SAFE to run locally with no paid model access and no GitHub secrets: the
# agent transform runs through the `fixture` executor — a deterministic mock agent
# (recorded edits, no model, no network). The exact same code path runs a real
# coding agent with `--executor local`; see the note at the end.
#
# Usage: demo/full-loop.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=../tests/fixtures/evals/lib.sh
source "$HERE/../tests/fixtures/evals/lib.sh"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
rule() { printf '%s\n' "------------------------------------------------------------"; }

eval_build_bin
echo

work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
base="$work/origins"; mono="$work/mono"

import_fixture="$REPO_ROOT/tests/fixtures/evals/api-migration"
export_fixture="$REPO_ROOT/tests/fixtures/evals/oss-export-scrub"

# ---------------------------------------------------------------------------
bold "STEP 1 — an upstream repo changes (acme/greeter renames Connect → New)"
rule
eval_load_meta "$import_fixture"
eval_setup_origin "$import_fixture" "$base" >/dev/null
git -C "$base/$REPO" log --oneline | sed 's/^/  upstream  /'
echo "  The v2 rename left an in-package caller (app/main.go) using the removed"
echo "  Connect() — the module no longer compiles. A coding agent must migrate it."

# The monorepo declares: import acme/greeter, then an agent_transform that ports
# callers, verified by 'go test ./...'. It also carries an export edge (sdk/go)
# committed up front, so STEP 4 can ship a path back out.
eval_setup_monorepo "$import_fixture" "$mono"
eval_overlay_monorepo "$export_fixture" "$mono" sdk/go
eval_load_meta "$import_fixture"
echo
echo "  Monorepo edge (//third_party/greeter:greeter):"
sed -n '/github_import/,/^)/p' "$mono/third_party/greeter/SRC" | sed 's/^/    /'

# ---------------------------------------------------------------------------
echo
bold "STEP 2 — capyfun import: mirror history, run the agent, VERIFY, materialize"
rule
echo "  agent loop: edit → \`$VERIFY\` → on failure feed stderr+diff back → retry → materialize"
echo
t0=$(now_ms)
run1_out="$(eval_import "$import_fixture" "$mono" "$base")"   # stderr (retry msgs) shown live
t1=$(now_ms)
run1_ms=$((t1 - t0))
echo "  $run1_out"
imported="$(sed -nE 's/.*imported ([0-9]+) commit.*/\1/p' <<<"$run1_out")"
misses="$(eval_misses "$run1_out")"

echo
echo "  Migrated caller (third_party/greeter/app/main.go):"
git -C "$mono" show main:third_party/greeter/app/main.go | grep -n 'greeter\.' | sed 's/^/    /'
echo
echo "  History — mirror commits carry CapyFun-Origin; the agent commit CapyFun-Agent:"
git -C "$mono" log main --format='    %h %s' | sed '/configure/d'

# ---------------------------------------------------------------------------
echo
bold "STEP 3 — verify the materialized result independently"
rule
verify_result="$(eval_verify_final "$mono" "$DEST" "$VERIFY")"
echo "  \`$VERIFY\` on third_party/greeter@main → $verify_result"

# ---------------------------------------------------------------------------
echo
bold "STEP 4 — export the other direction: ship a monorepo path out as a PR branch"
rule
eval_load_meta "$export_fixture"
dest="$(eval_export "$export_fixture" "$mono" "$base")"
export_branch="capyfun/export-${LABEL##*:}"
echo "  pushed branch $export_branch → $REPO (PR skipped: local hermetic destination)"
echo "  scrubbed client.go on the export branch (internal-only line deleted):"
git -C "$dest" show "$export_branch:client.go" | grep -n 'BaseURL' | sed 's/^/    /'

# ---------------------------------------------------------------------------
echo
bold "STEP 5 — replay: re-run the import. The agent does NOT run again."
rule
eval_load_meta "$import_fixture"
t2=$(now_ms)
replay_out="$(eval_import "$import_fixture" "$mono" "$base" 2>/dev/null)"
t3=$(now_ms)
replay_ms=$((t3 - t2))
echo "  $replay_out"
hits="$(eval_hits "$replay_out")"

# ---------------------------------------------------------------------------
echo
bold "================================  SUMMARY  ================================"
echo "Imported:            ${imported:-0} upstream commits (first-parent mirror, history preserved)"
echo "Agent:               $AGENT_DESC"
echo "Patch cache:         miss first run (${misses:-0} agent call), hit second run (${hits:-0} cache hit)"
echo "Verifier:            $verify_result   ($VERIFY, with one verify→retry loop available)"
echo "Export:              branch '$export_branch' produced (internal-only scrubbed)"
echo "First run time:      ${run1_ms} ms   (import + agent + verifier)"
echo "Replay time:         ${replay_ms} ms   (deterministic, cache-served)"
echo "Model calls (real):  0   — the fixture/mock agent makes no model calls"
echo "Estimated model cost: \$0.00 (mock).  The expensive model only runs on a"
echo "                     content-addressed cache MISS; every replay is free."
echo "=========================================================================="
echo
echo "Run the REAL agent path (needs a logged-in claude/codex/agy CLI or an API key):"
echo "  capyfun import //third_party/greeter:greeter --root <monorepo> --executor local"
echo "Measured eval table across 3 fixtures:  scripts/eval-agents.sh"
