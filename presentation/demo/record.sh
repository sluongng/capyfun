#!/usr/bin/env bash
#
# Record the two CapyFun demos:
#   presentation/demo-imperative.cast  - structural transforms + tip patch (hermetic)
#   presentation/demo-generative.cast  - a live agent_transform (needs claude + net)
#
# The .cast files play offline. Requires: asciinema, tmux, tree, watch
# (and a logged-in `claude` for the generative one).
#
# Usage: presentation/demo/record.sh [imperative|generative|all]
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
WHICH="${1:-all}"

echo "==> building capyfun"
cargo build --quiet --manifest-path "$REPO_ROOT/Cargo.toml"
export CAPYFUN_BIN="$REPO_ROOT/target/debug/capyfun"

record_one() {
    local mode="$1" out="$REPO_ROOT/presentation/demo-$1.cast"
    echo "==> recording $mode -> $out"
    tmux -L capyfundemo kill-server 2>/dev/null || true
    asciinema rec "$out" \
        --overwrite --headless --window-size 200x40 \
        --output-format asciicast-v2 \
        --idle-time-limit 2.5 \
        --title "CapyFun - $mode transform" \
        -c "bash '$HERE/drive.sh' '$mode'"

    # Strip tmux's teardown tail (alt-screen exit / clear / "[exited]") so the
    # cast ends on the clean final frame instead of a blanked/garbled screen.
    python3 - "$out" <<'PY'
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
}

case "$WHICH" in
    imperative) record_one imperative ;;
    generative) record_one generative ;;
    all)        record_one imperative; record_one generative ;;
    *) echo "usage: record.sh [imperative|generative|all]" >&2; exit 2 ;;
esac

echo "==> done"
