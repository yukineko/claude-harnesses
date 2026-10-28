#!/usr/bin/env bash
# Repro for GitHub issue 1345: the fugu-router SessionEnd hook
# (harvest; sync, timeout 60) is reported "Hook cancelled" at the end of a
# headless claude -p run.
#
# Observed 2026-10-25 on Claude Code 2.1.284, installed fugu-router 0.1.60,
# loaded alone via --setting-sources local --plugin-dir: stderr carried
#   SessionEnd hook [${CLAUDE_PLUGIN_ROOT}/bin/fugu-router harvest; ${CLAUDE_PLUGIN_ROOT}/bin/fugu-router sync] failed: Hook cancelled
# and the debug log showed the cancel about 1.5 s after shutdown began, far
# short of the declared 60 s timeout. sync had made its local commit (records
# not lost locally) but the network pull/push never completed, so the run's
# records were not pushed to the sync remote.
#
# Revision dependence: the verdict depends on the checked-out tree this script
# lives in. It runs `cargo build -p fugu-router` from that tree, stages a plugin
# dir in a scratch dir (.claude-plugin/, hooks/, the bin/fugu-router launcher and
# the freshly built binary under the name the launcher execs,
# bin/fugu-router-OS-ARCH) and loads only that staged dir via --plugin-dir.
# It does NOT read ~/.claude/plugins/cache, so an installed (possibly stale)
# plugin cannot decide the result.
#
# Hermetic sync remote (so the verdict does not depend on machine state, e.g.
# on how many commits the real ~/.fugu-router/sync happens to be ahead):
# fugu-router finds its config and sync checkout only through $HOME
# (config::home_dir, i.e. dirs::home_dir, which reads $HOME). The probe builds
# a sandbox HOME in its scratch dir holding .fugu-router/config.toml, a sync
# checkout one commit ahead of its upstream, and that upstream: a local bare
# repo whose pre-receive hook sleeps SLOW seconds. So every sync must push,
# and every push takes SLOW seconds, far past the ~1.5 s SessionEnd budget.
# claude itself keeps the real HOME (it needs it for its login); only the
# hooks get the sandbox HOME, through `--settings '{"env":{"HOME":...}}'`.
# The real ~/.fugu-router is never touched; its HEAD is printed before and
# after as evidence.
#
# Controls (each failure is UNDETERMINED, exit 126, never OK):
#   - slow control: a timed push of a throwaway ref to the sandbox remote must
#     take at least SLOW-1 s, i.e. the sandbox really is slow;
#   - the pre-receive hook logs each push it receives, so the probe can show
#     whether the hook's sync reached the push at all.
#
# Verdict:
#   DEFECT (exit 1) when the SessionEnd hook is reported cancelled, or when it
#     is not cancelled but the sync checkout's commit never lands in the
#     sandbox remote within WAIT seconds (records not pushed).
#   OK (exit 0) when the hook is not cancelled AND the commit lands in the
#     remote (the detached worker finished the push after claude exited).
#
# Side effects: none outside the scratch dir. The scratch dir (sandbox HOME,
# git repos) is left under ${TMPDIR:-/tmp} and its path is printed; only the
# staged plugin and the claude output files are removed.
#
# Exit codes:
#   0   no defect (the SessionEnd hook is not cancelled and the push lands)
#   1   defect present (cancelled, or the records never reach the remote)
#   126 cannot determine (claude, cargo or git missing, cargo build failed,
#       plugin sources missing from the tree, sandbox setup failed, the
#       sandbox remote is not slow, claude failed)
# Cost: one `cargo build -p fugu-router` (debug profile; incremental), one
# `claude -p --model haiku` call, about SLOW seconds of control push and up to
# WAIT seconds of waiting for the detached worker.
set -u

SLOW=10
WAIT=60

# Final pytest/cargo-style summary line. It only reports the exit status
# the checks below chose (0 OK -> "1 passed", 1 DEFECT -> "1 failed",
# anything else, i.e. undetermined -> "0 passed"); it never changes it.
summary() {
  case "$1" in 0) echo "1 passed" ;; 1) echo "1 failed" ;; *) echo "0 passed" ;; esac
}
trap 'summary $?' EXIT

command -v claude >/dev/null 2>&1 || { echo "UNDETERMINED: claude not on PATH"; exit 126; }
command -v git >/dev/null 2>&1 || { echo "UNDETERMINED: git not on PATH"; exit 126; }

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
src="$REPO/crates/fugu-router"
for f in .claude-plugin hooks bin/fugu-router; do
  [ -e "$src/$f" ] || { echo "UNDETERMINED: $src/$f missing in the checked-out tree"; exit 126; }
done

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
command -v cargo >/dev/null 2>&1 || { echo "UNDETERMINED: cargo not on PATH"; exit 126; }
cargo build --manifest-path "$REPO/Cargo.toml" -p fugu-router >&2 \
  || { echo "UNDETERMINED: cargo build -p fugu-router failed"; exit 126; }
built="${CARGO_TARGET_DIR:-$REPO/target}/debug/fugu-router"
[ -x "$built" ] || { echo "UNDETERMINED: built binary not found at $built"; exit 126; }

plugin=""
tmp="$(mktemp -d "${TMPDIR:-/tmp}/gh1345.XXXXXX")" || exit 126
cleanup() {
  rc=$?
  rm -f "$tmp/debug.log" "$tmp/stdout" "$tmp/stderr"
  if [ -n "$plugin" ]; then
    rm -f "$plugin"/bin/fugu-router* "$plugin"/hooks/* "$plugin"/.claude-plugin/*
    rmdir "$plugin/bin" "$plugin/hooks" "$plugin/.claude-plugin" "$plugin" 2>/dev/null
  fi
  echo "scratch dir (sandbox HOME and git repos) left at: $tmp"
  summary "$rc"
}
trap cleanup EXIT

# Stage the plugin from THIS tree. The binary name is computed exactly as the
# bin/fugu-router launcher computes it.
case "$(uname -s)" in
  Linux)  os=linux ;;
  Darwin) os=darwin ;;
  MINGW*|MSYS*|CYGWIN*) os=windows ;;
  *)      os="$(uname -s)" ;;
esac
case "$(uname -m)" in
  x86_64|amd64)   arch=x86_64 ;;
  arm64|aarch64)  arch=arm64 ;;
  *)              arch="$(uname -m)" ;;
esac
ext=""
[ "$os" = windows ] && ext=".exe"

plugin="$tmp/plugin"
mkdir -p "$plugin/bin" || exit 126
cp -R "$src/.claude-plugin" "$src/hooks" "$plugin/" || { echo "UNDETERMINED: staging plugin dirs failed"; exit 126; }
cp "$src/bin/fugu-router" "$plugin/bin/fugu-router" || { echo "UNDETERMINED: staging launcher failed"; exit 126; }
cp "$built" "$plugin/bin/fugu-router-$os-$arch$ext" || { echo "UNDETERMINED: staging binary failed"; exit 126; }
chmod +x "$plugin/bin/fugu-router" "$plugin/bin/fugu-router-$os-$arch$ext"
[ -f "$plugin/hooks/hooks.json" ] || { echo "UNDETERMINED: staged plugin has no hooks/hooks.json"; exit 126; }

# --- hermetic sandbox HOME with a slow sync remote ---
real_home="$HOME"
sandbox="$tmp/home"
bare="$tmp/remote.git"
sync="$sandbox/.fugu-router/sync"
prelog="$tmp/pre-receive.log"
setup_fail() { echo "UNDETERMINED: sandbox setup failed: $1"; exit 126; }
mkdir -p "$sandbox/.fugu-router" || setup_fail "mkdir $sandbox"
# Git config for every git call below AND for the hooks' git (they inherit
# this env): no system config, sandbox global config only.
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL="$sandbox/.gitconfig"
git config --file "$GIT_CONFIG_GLOBAL" user.name gh1345-probe || setup_fail "gitconfig"
git config --file "$GIT_CONFIG_GLOBAL" user.email gh1345-probe@example.invalid || setup_fail "gitconfig"
git config --file "$GIT_CONFIG_GLOBAL" init.defaultBranch main || setup_fail "gitconfig"
git init -q --bare "$bare" || setup_fail "git init --bare"
git clone -q "$bare" "$sync" 2>/dev/null || setup_fail "git clone"
echo '{"seed":1}' >"$sync/episodes.jsonl" || setup_fail "seed write"
git -C "$sync" add episodes.jsonl && git -C "$sync" commit -q -m seed \
  && git -C "$sync" push -q -u origin main || setup_fail "seed push"
echo '{"probe":"gh1345 record not yet pushed"}' >>"$sync/episodes.jsonl" || setup_fail "append"
git -C "$sync" commit -q -am 'gh1345 probe: local record ahead of upstream' || setup_fail "ahead commit"
# Slow remote: every push sleeps SLOW seconds in pre-receive. Each push is
# logged so the probe can see whether the hook's sync reached the push.
cat >"$bare/hooks/pre-receive" <<EOF || setup_fail "pre-receive hook"
#!/bin/sh
refs="\$(cat)"
echo "push received: \$refs" >>"$prelog"
sleep $SLOW
exit 0
EOF
chmod +x "$bare/hooks/pre-receive" || setup_fail "chmod pre-receive"
cat >"$sandbox/.fugu-router/config.toml" <<EOF || setup_fail "config.toml"
sync_repo = "$bare"
sync_dir  = "$sync"
EOF

# Slow control: the sandbox remote must really be slow, or the run cannot
# discriminate a hook that fits the budget from one that does not.
t0=$(date +%s)
git -C "$sync" push -q origin HEAD:refs/probe/control 2>/dev/null || setup_fail "control push"
t1=$(date +%s)
echo "slow control: a push to the sandbox remote took $((t1 - t0)) s (pre-receive sleeps $SLOW s)"
[ $((t1 - t0)) -ge $((SLOW - 1)) ] || { echo "UNDETERMINED: the sandbox remote is not slow; control push took $((t1 - t0)) s"; exit 126; }

ahead() { git -C "$sync" rev-list --count '@{u}..HEAD' 2>/dev/null || echo unknown; }
pushes() { [ -f "$prelog" ] && wc -l <"$prelog" | tr -d ' ' || echo 0; }
remote_main() { git -C "$bare" rev-parse -q --verify refs/heads/main 2>/dev/null || echo none; }
real_sync_head() { git -C "$real_home/.fugu-router/sync" rev-parse HEAD 2>/dev/null || echo none; }
pending_markers() { ls "$sandbox/.fugu-router/session-end-pending" 2>/dev/null | wc -l | tr -d ' '; }

echo "repo rev: $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "built fugu-router version: $(sed -n 's/^version *= *"\(.*\)"/\1/p' "$src/Cargo.toml" | head -1) ($built)"
echo "staged plugin: $plugin"
echo "sandbox HOME for hooks: $sandbox"
echo "real ~/.fugu-router/sync HEAD before: $(real_sync_head)"
echo "sandbox sync commits ahead of upstream before: $(ahead)"
echo "sandbox remote pushes received before: $(pushes)"
target="$(git -C "$sync" rev-parse HEAD)"
echo "commit that must reach the remote (it or a descendant): $target"

cd "$tmp" || exit 126
claude -p 'Reply with the single word READY.' --model haiku --setting-sources local \
  --settings "{\"env\":{\"HOME\":\"$sandbox\"}}" \
  --plugin-dir "$plugin" --debug-file "$tmp/debug.log" --verbose \
  </dev/null >"$tmp/stdout" 2>"$tmp/stderr"
rc=$?
echo "claude --version: $(claude --version)"
echo "claude exit: $rc"
echo "stdout: $(cat "$tmp/stdout")"
echo "stderr: $(cat "$tmp/stderr")"
echo "debug SessionEnd lines:"
grep 'SessionEnd' "$tmp/debug.log"
echo "right after claude exit: remote main $(remote_main), pushes received $(pushes)"

[ "$rc" -eq 0 ] || { echo "UNDETERMINED: claude -p exited $rc"; exit 126; }
grep -q 'SessionEnd' "$tmp/debug.log" || { echo "UNDETERMINED: no SessionEnd hook activity in the debug log"; exit 126; }

cancelled=0
if grep -q 'fugu-router sync\] failed: Hook cancelled' "$tmp/stderr" \
   || grep -qE 'SessionEnd.*fugu-router.*cancelled' "$tmp/debug.log"; then
  cancelled=1
fi

# Wait for the commit to land (a detached worker pushes after claude exits)
# and for the worker to drop its pending marker. A cancelled hook is waited
# for too, so the record shows whether its push ever landed.
landed=0
waited=0
while [ "$waited" -lt "$WAIT" ]; do
  # sync may add its own commit (e.g. .gitattributes) on top of $target, so
  # "landed" means $target is contained in the remote main.
  if git -C "$bare" merge-base --is-ancestor "$target" refs/heads/main 2>/dev/null \
     && [ "$(pending_markers)" = 0 ]; then
    landed=1
    break
  fi
  [ "$cancelled" = 1 ] && [ "$waited" -ge $((SLOW + 5)) ] && break
  sleep 1
  waited=$((waited + 1))
done
echo "after waiting ${waited} s: remote main $(remote_main), pushes received $(pushes), pending markers $(pending_markers), record landed in remote: $([ "$landed" = 1 ] && echo yes || echo no)"
echo "sandbox sync commits ahead of upstream after: $(ahead)"
echo "real ~/.fugu-router/sync HEAD after: $(real_sync_head)"
[ -f "$sandbox/.fugu-router/session-end.log" ] && { echo "session-end.log:"; cat "$sandbox/.fugu-router/session-end.log"; }
[ -f "$sandbox/.fugu-router/sync-error.json" ] && { echo "sync-error.json:"; cat "$sandbox/.fugu-router/sync-error.json"; echo; }

if [ "$cancelled" = 1 ]; then
  echo "DEFECT: fugu-router SessionEnd hook was cancelled in a headless run"
  exit 1
fi
if [ "$landed" != 1 ]; then
  echo "DEFECT: fugu-router SessionEnd hook was not cancelled but the local record never reached the sync remote within $WAIT s"
  exit 1
fi
echo "OK: fugu-router SessionEnd hook was not cancelled and the local record reached the sync remote"
exit 0
