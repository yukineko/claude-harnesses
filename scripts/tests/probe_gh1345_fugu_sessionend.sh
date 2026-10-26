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
# Side effect: the staged plugin runs from a non-git scratch dir, so harvest is
# a no-op, and sync acts on ~/.fugu-router/sync exactly as any session end does
# (local commit of pending records, pull, push).
#
# Exit codes:
#   0   no defect (the SessionEnd hook is not cancelled)
#   1   defect present (the fugu-router SessionEnd hook is cancelled)
#   126 cannot determine (claude or cargo missing, cargo build failed, plugin
#       sources missing from the tree, claude failed)
# Cost: one `cargo build -p fugu-router` (debug profile; incremental) plus one
# `claude -p --model haiku` call.
set -u

# Final pytest/cargo-style summary line. It only reports the exit status
# the checks below chose (0 OK -> "1 passed", 1 DEFECT -> "1 failed",
# anything else, i.e. undetermined -> "0 passed"); it never changes it.
summary() {
  case "$1" in 0) echo "1 passed" ;; 1) echo "1 failed" ;; *) echo "0 passed" ;; esac
}
trap 'summary $?' EXIT

command -v claude >/dev/null 2>&1 || { echo "UNDETERMINED: claude not on PATH"; exit 126; }

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
  rmdir "$tmp" 2>/dev/null
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

ahead() { git -C "$HOME/.fugu-router/sync" rev-list --count '@{u}..HEAD' 2>/dev/null || echo unknown; }
echo "repo rev: $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "built fugu-router version: $(sed -n 's/^version *= *"\(.*\)"/\1/p' "$src/Cargo.toml" | head -1) ($built)"
echo "staged plugin: $plugin"
echo "sync repo commits ahead of upstream before: $(ahead)"

cd "$tmp" || exit 126
claude -p 'Reply with the single word READY.' --model haiku --setting-sources local \
  --plugin-dir "$plugin" --debug-file "$tmp/debug.log" --verbose \
  </dev/null >"$tmp/stdout" 2>"$tmp/stderr"
rc=$?
echo "claude --version: $(claude --version)"
echo "claude exit: $rc"
echo "stdout: $(cat "$tmp/stdout")"
echo "stderr: $(cat "$tmp/stderr")"
echo "debug SessionEnd lines:"
grep 'SessionEnd' "$tmp/debug.log"
echo "sync repo commits ahead of upstream after: $(ahead)"

[ "$rc" -eq 0 ] || { echo "UNDETERMINED: claude -p exited $rc"; exit 126; }
if grep -q 'fugu-router sync\] failed: Hook cancelled' "$tmp/stderr" \
   || grep -qE 'SessionEnd.*fugu-router.*cancelled' "$tmp/debug.log"; then
  echo "DEFECT: fugu-router SessionEnd hook was cancelled in a headless run"
  exit 1
fi
grep -q 'SessionEnd' "$tmp/debug.log" || { echo "UNDETERMINED: no SessionEnd hook activity in the debug log"; exit 126; }
echo "OK: fugu-router SessionEnd hook was not cancelled"
exit 0
