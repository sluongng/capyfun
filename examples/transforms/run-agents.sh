#!/usr/bin/env bash
#
# Smoke-run the agents declared in tools/agent/SRC through `capyfun agent-run`.
# Each agent pairs a harness with a model (mirrored here until the agent/harness/
# model config rules are wired into the engine — T4):
#
#   reviewer    claude_code + opus        (anthropic)  -> claude CLI login
#   triager     antigravity + gemini_flash(google)     -> agy CLI login
#   porter      codex       + gpt55       (openai)     -> codex CLI login
#   modernizer  pi          + nemotron    (nebius)     -> NEBIUS_API_KEY (HTTP)
#
# claude_code, codex, and antigravity use their local CLI logins (no key needed).
# The pi harness calls Nebius's OpenAI-compatible endpoint and needs NEBIUS_API_KEY.
#
# Credentials: export NEBIUS_API_KEY, or put it in examples/transforms/secrets.env
# (gitignored) as `NEBIUS_API_KEY=...`. Agents whose credentials are absent are
# skipped, not failed.
#
# Usage: examples/transforms/run-agents.sh
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

# Load local secrets if present (gitignored).
if [ -f "$here/secrets.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$here/secrets.env"
    set +a
fi

echo "==> building capyfun"
cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
bin="$repo_root/target/debug/capyfun"

PROMPT="Reply with exactly one word, lowercase, no punctuation: capybara"
# The model id Nebius Token Factory serves; override if your deployment differs.
NEBIUS_MODEL="${NEBIUS_MODEL:-nvidia/Nemotron-3-Ultra-550b-a55b}"

run() {
    local name="$1"; shift
    echo
    echo "==> $name: $*"
    if "$bin" agent-run "$@" --prompt "$PROMPT"; then
        echo "    [ok]"
    else
        echo "    [FAILED]"
        return 1
    fi
}

rc=0

run reviewer --harness claude_code --provider anthropic --model claude-opus-4-8 || rc=1
run triager  --harness antigravity --provider google     --model "Gemini 3.5 Flash (High)" || rc=1
run porter   --harness codex       --provider openai     --model gpt-5.5         || rc=1

if [ -n "${NEBIUS_API_KEY:-}" ]; then
    run modernizer --harness pi --provider nebius --model "$NEBIUS_MODEL" || rc=1
else
    echo
    echo "==> modernizer (pi + nemotron): SKIPPED — NEBIUS_API_KEY not set"
    echo "    export it or add it to $here/secrets.env to test the Nebius path"
fi

echo
[ "$rc" -eq 0 ] && echo "DONE" || echo "DONE (some agents failed)"
exit "$rc"
