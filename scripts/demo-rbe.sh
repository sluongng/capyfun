#!/usr/bin/env bash
# Demo: CapyFun generative transforms on BuildBuddy Cloud RBE.
#
# Two modes:
#
#   ./scripts/demo-rbe.sh smoke   (default)
#       Run the REAPI mechanism end-to-end against real BuildBuddy Cloud using a
#       stock busybox image: a CAS round-trip and a remote `sh` action whose
#       output subtree is materialized back and whose re-run is an Action Cache
#       hit. Proves upload -> Execute -> materialize -> AC cache against the real
#       service. No custom image needed.
#
#   ./scripts/demo-rbe.sh agent
#       Print the steps to run a REAL agent_transform on RBE (custom claude image
#       + ANTHROPIC_API_KEY as a BuildBuddy secret), then run
#       `capyfun import --executor remote` twice to show a cache miss then an
#       Action Cache hit. Requires BUILDBUDDY_EXEC_IMAGE to point at a
#       claude-capable image (see demo/rbe/Dockerfile.agent).
#
# Auth: set BUILDBUDDY_API_KEY (CapyFun -> BuildBuddy, sent as the
# x-buildbuddy-api-key gRPC header). The key is read at runtime and never stored
# in the repo.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-smoke}"

if [[ -z "${BUILDBUDDY_API_KEY:-}" ]]; then
  echo "error: set BUILDBUDDY_API_KEY (your BuildBuddy org API key)." >&2
  echo "       e.g. export BUILDBUDDY_API_KEY=xxxxxxxx" >&2
  exit 1
fi

case "$MODE" in
  smoke)
    echo "==> Live REAPI mechanism against BuildBuddy Cloud (busybox image)"
    echo "    endpoint: ${BUILDBUDDY_ENDPOINT:-grpcs://remote.buildbuddy.io}"
    echo
    # The gated live tests ARE the smoke demo: a CAS round-trip and a remote sh
    # action with output materialization + an AC cache hit.
    cargo test --lib --quiet -- \
      remote::client::tests::live_cas_and_ac_roundtrip \
      remote::executor::tests::live_remote_execution_and_cache \
      --nocapture
    echo
    echo "==> OK: blobs round-tripped, a remote action executed on RBE, its output"
    echo "        subtree was materialized back, and the re-run hit the Action Cache."
    echo "    Upgrade to a real agent: ./scripts/demo-rbe.sh agent"
    ;;

  agent)
    : "${BUILDBUDDY_EXEC_IMAGE:?set BUILDBUDDY_EXEC_IMAGE=docker://<registry>/capyfun-agent:latest (see demo/rbe/Dockerfile.agent)}"
    ROOT="${CAPYFUN_DEMO_ROOT:-demo}"
    TARGET="${CAPYFUN_DEMO_TARGET:-//third_party/widget:widget}"

    cat <<EOF
==> Real agent_transform on BuildBuddy RBE
    image:   $BUILDBUDDY_EXEC_IMAGE
    monorepo: $ROOT
    target:   $TARGET

Prerequisites (one-time):
  1. Build + push the agent image:
       docker build -f demo/rbe/Dockerfile.agent -t <registry>/capyfun-agent:latest .
       docker push <registry>/capyfun-agent:latest
  2. In the BuildBuddy UI, add an org secret named ANTHROPIC_API_KEY. BuildBuddy
     injects it into the action as an env var at execution time, so it never
     enters the Action digest.

Running the import with the remote executor (cache MISS -> agent runs on a worker):
EOF
    cargo run --quiet -- import "$TARGET" --root "$ROOT" --executor remote

    echo
    echo "Re-running (identical Action digest -> Action Cache HIT, no agent run):"
    cargo run --quiet -- import "$TARGET" --root "$ROOT" --executor remote
    echo
    echo "==> Look for 'cache 1h/0m' (or higher) on the second run: the agent output"
    echo "        was served from BuildBuddy's Action Cache."
    ;;

  *)
    echo "usage: $0 [smoke|agent]" >&2
    exit 2
    ;;
esac
