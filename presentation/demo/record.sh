#!/usr/bin/env bash
#
# Record the CapyFun import+transform demo to presentation/demo.cast.
# Hermetic (no network) and reproducible. Requires: asciinema, tmux, tree, watch.
#
# Usage: presentation/demo/record.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

echo "==> building capyfun"
cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml"
export CAPYFUN_BIN="$REPO_ROOT/target/debug/capyfun"

OUT="$REPO_ROOT/presentation/demo.cast"
echo "==> recording -> $OUT"
# asciicast-v2: the format asciinema-player (used by presentation/demo.html)
# can load. asciinema 3.x records v3 by default, which the player can't read.
asciinema rec "$OUT" \
  --overwrite --headless --window-size 200x48 \
  --output-format asciicast-v2 \
  --idle-time-limit 2.5 \
  --title "CapyFun - import + transform a dependency into the monorepo" \
  -c "bash '$HERE/drive.sh'"

echo "==> done: $OUT"
echo "    play:  asciinema play '$OUT'"
