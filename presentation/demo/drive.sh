#!/usr/bin/env bash
#
# The command asciinema records: build a hermetic workspace, open a two-pane
# tmux (left = live file tree, right = scripted commands), drive the right pane
# with pacing, then end. Not meant to be run directly — see record.sh.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
BIN="${CAPYFUN_BIN:-$REPO_ROOT/target/debug/capyfun}"
S=capyfun

W="$(mktemp -d /tmp/capyfun-demo.XXXXXX)"
trap 'tmux -L capyfundemo kill-server 2>/dev/null || true; rm -rf "$W"' EXIT

MONO="$(bash "$HERE/setup.sh" "$W" | tail -1)"
ORIGINS="$W/origins"

# rcfile for the right (command) pane: clean prompt, capyfun on PATH, hermetic origin.
cat > "$W/right.rc" <<RC
cd '$MONO'
export CAPYFUN_GITHUB_BASE='$ORIGINS'
export PATH='$(dirname "$BIN")':"\$PATH"
export GIT_PAGER=cat PAGER=cat
PS1='\[\033[1;32m\]capyfun-demo\[\033[0m\]\$ '
clear
RC

# Private tmux server (own socket + default config) so the demo is immune to the
# user's tmux base-index / keybindings / status styling.
TM="tmux -L capyfundemo"
$TM kill-server 2>/dev/null || true
$TM -f /dev/null new-session -d -s "$S" -x 200 -y 48
$TM set -t "$S" status off

# left pane: live file-tree watcher
$TM send-keys -t "$S:0.0" "clear; watch -t -c -n 0.4 \"bash '$HERE/treepane.sh' '$MONO'\"" Enter
# right pane: interactive bash with our rcfile
$TM split-window -h -t "$S:0.0" -l 62% "bash --rcfile '$W/right.rc' -i"

# director: type the demo into the right pane with pacing, then end the session
(
  RP="$S:0.1"
  send(){ $TM send-keys -t "$RP" "$1" Enter; }
  sleep 2.5
  send "# 1) the import + its transforms, declared as code"
  sleep 1.2; send "cat third_party/widget/SRC"; sleep 6
  send "# 2) upstream BEFORE — code in pkg/, imports acme.internal/log"
  sleep 1.2; send "git -C '$ORIGINS/acme/widget' show main:pkg/widget.go | sed -n '4,7p'"; sleep 5.5
  send "# 3) import it — transforms run as the mirror is built"
  sleep 1.2; send "capyfun import //third_party/widget:widget"; sleep 3.5
  send "# materialize the new tip into the working tree (watch the LEFT pane)"
  sleep 1.2; send "git reset --hard main"; sleep 4.5
  send "# 4) AFTER — moved pkg/ -> lib/, 'acme.internal/' scrubbed"
  sleep 1.2; send "git show main:third_party/widget/lib/widget.go | sed -n '4,7p'"; sleep 5.5
  send "# 5) go.mod pinned by the tip apply_patch"
  sleep 1.2; send "git show main:third_party/widget/go.mod"; sleep 5
  send "# 6) every commit maps back upstream via CapyFun-Origin"
  sleep 1.2; send "git log main --format='%C(yellow)%h%C(reset) %s%n   %(trailers:key=CapyFun-Origin)' | sed '/^ *\$/d' | head -8"; sleep 7
  send "# history preserved - every file traceable - reproducible."
  sleep 4
  $TM kill-session -t "$S"
) &

$TM attach -t "$S"
