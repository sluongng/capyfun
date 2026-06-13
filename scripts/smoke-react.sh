#!/usr/bin/env bash
#
# Hermetic smoke test for `capyfun react` (issue -> agent -> PR). Builds a local
# bare origin for acme/backend, stubs the agent harness with a fake `claude` that
# edits the checkout, runs the reaction through the real CLI (LocalForge via
# CAPYFUN_GITHUB_BASE), and asserts a prototype branch was pushed with the agent's
# change and the provenance trailers. No network, no real agent. Rerunnable.
#
# Usage: scripts/smoke-react.sh
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
echo "==> building capyfun"
cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
bin="$repo_root/target/debug/capyfun"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export GIT_AUTHOR_NAME=smoke GIT_AUTHOR_EMAIL=smoke@example.com
export GIT_COMMITTER_NAME=smoke GIT_COMMITTER_EMAIL=smoke@example.com

fail() { echo "FAIL: $*" >&2; exit 1; }

# --- fake agent harness: a `claude` that edits the checkout in place ---
mkdir -p "$work/bin"
cat > "$work/bin/claude" <<'EOF'
#!/usr/bin/env bash
# Ignore all args (prompt/model/flags); just make a reviewable change in CWD.
cat > VERSION.md <<'MD'
# Version endpoint

GET /version returns {"version": "..."} — prototyped by the agent.
MD
EOF
chmod +x "$work/bin/claude"

# --- bare origin (stands in for github.com/acme/backend) on `main` ---
base="$work/origins"
origin="$base/acme/backend"
mkdir -p "$origin"
git -C "$origin" init -q --bare -b main
seed="$work/seed"
git -C "$work" init -q -b main seed
printf 'backend\n' > "$seed/README.md"
git -C "$seed" add -A
git -C "$seed" commit -qm init
git -C "$seed" push -q "$origin" main

echo "==> dry-run (resolve + match only)"
"$bin" react --issue "$repo_root/testdata/issue-labeled.json" \
  --root "$repo_root/examples/reactions" --dry-run \
  | grep -q "branch: capyfun/issue-7" || fail "dry-run did not resolve the reaction"

echo "==> live reaction (fake agent + local forge)"
out="$(CAPYFUN_GITHUB_BASE="$base" PATH="$work/bin:$PATH" \
  "$bin" react --issue "$repo_root/testdata/issue-labeled.json" \
  --root "$repo_root/examples/reactions")"
echo "$out"
echo "$out" | grep -q "recorded PR on acme/backend" || fail "no PR was recorded"

# --- assert the prototype branch landed on the origin ---
git -C "$origin" rev-parse --verify capyfun/issue-7 >/dev/null 2>&1 \
  || fail "prototype branch not pushed to origin"
git -C "$origin" ls-tree --name-only capyfun/issue-7 | grep -q "VERSION.md" \
  || fail "agent's change missing from the branch"
git -C "$origin" log -1 --format=%B capyfun/issue-7 | grep -q "CapyFun-Agent:" \
  || fail "CapyFun-Agent trailer missing"
git -C "$origin" log -1 --format=%B capyfun/issue-7 | grep -q "CapyFun-Issue: acme/backend#7" \
  || fail "CapyFun-Issue trailer missing"

echo "PASS"
