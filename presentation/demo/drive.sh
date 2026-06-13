#!/usr/bin/env bash
#
# The command asciinema records: build a hermetic workspace, open a two-pane
# tmux (left = SRC config + live file tree, right = scripted commands), drive
# the right pane with pacing, then end. Not run directly — see record.sh.
#
# Usage: drive.sh <imperative|generative>
set -euo pipefail
MODE="${1:?usage: drive.sh <imperative|generative>}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
BIN="${CAPYFUN_BIN:-$REPO_ROOT/target/debug/capyfun}"
S=capyfun

W="$(mktemp -d /tmp/capyfun-demo.XXXXXX)"
trap 'tmux -L capyfundemo kill-server 2>/dev/null || true; rm -rf "$W"' EXIT

MONO="$(bash "$HERE/setup.sh" "$W" "$MODE" | tail -1)"
ORIGINS="$W/origins"

cat > "$W/right.rc" <<RC
cd '$MONO'
export CAPYFUN_GITHUB_BASE='$ORIGINS'
export PATH='$(dirname "$BIN")':"\$PATH"
export GIT_PAGER=cat PAGER=cat
PS1='\[\033[1;32m\]capyfun-demo\[\033[0m\]\$ '
clear
RC

# Private tmux server (own socket + default config), immune to the user's tmux.
TM="tmux -L capyfundemo"
$TM kill-server 2>/dev/null || true
$TM -f /dev/null new-session -d -s "$S" -x 200 -y 40
$TM set -t "$S" status off

# left pane: SRC config + live tree
$TM send-keys -t "$S:0.0" "clear; watch -t -c -n 0.4 \"bash '$HERE/treepane.sh' '$MONO' '$MODE'\"" Enter
# right pane: interactive bash with our rcfile
$TM split-window -h -t "$S:0.0" -l 55% "bash --rcfile '$W/right.rc' -i"

(
  RP="$S:0.1"
  send(){ $TM send-keys -t "$RP" "$1" Enter; }
  cl(){ $TM send-keys -t "$RP" "clear" Enter; sleep 0.5; }

  sleep 2.5
  if [ "$MODE" = imperative ]; then
    send "# the LEFT pane shows the SRC: replace + move + apply_patch"; sleep 4
    cl
    send "# upstream BEFORE — code in pkg/, imports acme.internal/log"
    sleep 1.2; send "git -C '$ORIGINS/acme/widget' show main:pkg/widget.go | sed -n '4,7p'"; sleep 5.5
    cl
    send "# import — structural transforms per commit + the tip patch"
    sleep 1.2; send "capyfun import //third_party/widget:widget"; sleep 3.5
    send "git reset --hard main >/dev/null; echo '   -> materialized (watch the LEFT pane)'"; sleep 5
    cl
    send "# provenance — every mirror commit maps back upstream"
    sleep 1.2; send "git log main --format='%C(yellow)%h%C(reset) %s%n   %(trailers:key=CapyFun-Origin)' | sed '/^ *\$/d' | head -8"; sleep 8
  else
    send "# agents are composable config — harness x model -> agent"
    sleep 1.2; send "cat tools/harness/SRC tools/models/SRC tools/agent/SRC"; sleep 6
    cl
    send "# import — the agent_transform (LEFT pane) runs over the landed code"
    sleep 1.2; send "capyfun import //third_party/widget:widget"; sleep 18
    send "git reset --hard main >/dev/null; echo '   -> 1 agent commit; materialized (watch LEFT)'"; sleep 5
    cl
    send "# provenance — CapyFun-Agent records the generative step"
    sleep 1.2; send "git log main --format='%C(yellow)%h%C(reset) %s%n   %(trailers:key=CapyFun-Agent)' | sed '/^ *\$/d' | head -6"; sleep 6
    cl
    send "# reproducible — re-import replays the cached patch (no re-run)"
    sleep 1.2; send "capyfun import //third_party/widget:widget"; sleep 6
  fi
  send "# done."
  sleep 3
  $TM kill-session -t "$S"
) &

$TM attach -t "$S"
