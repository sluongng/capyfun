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

# Strip tmux's teardown tail (alt-screen exit / clear / "[exited]") emitted when
# the session is killed, so the recording ends on the clean final frame instead
# of a blanked/garbled screen. End with a cursor-show for good measure.
echo "==> trimming teardown tail"
python3 - "$OUT" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as f:
    header = f.readline()
    evs = [json.loads(l) for l in f if l.strip()]
def teardown(d): return ('\x1b[?1049l' in d) or ('[exited]' in d)
cut = next((i for i, e in enumerate(evs) if teardown(e[2])), len(evs))
kept = evs[:cut]
kept.append([kept[-1][0] if kept else 0.0, "o", "\x1b[?25h"])
with open(path, "w") as f:
    f.write(header)
    for e in kept:
        f.write(json.dumps(e) + "\n")
print(f"    kept {len(kept)} events (dropped {len(evs)-cut} teardown)")
PY

echo "==> done: $OUT"
echo "    play:  asciinema play '$OUT'"
