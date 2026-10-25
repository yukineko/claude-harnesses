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
# Side effect: this runs the REAL installed plugin from a non-git scratch dir,
# so harvest is a no-op, and sync acts on ~/.fugu-router/sync exactly as any
# session end does (local commit of pending records, pull, push).
#
# Exit codes:
#   0   no defect (the SessionEnd hook is not cancelled)
#   1   defect present (the fugu-router SessionEnd hook is cancelled)
#   126 cannot determine (claude or the installed plugin missing, claude failed)
# Cost: one `claude -p --model haiku` call.
set -u

# Final pytest/cargo-style summary line. It only reports the exit status
# the checks below chose (0 OK -> "1 passed", 1 DEFECT -> "1 failed",
# anything else, i.e. undetermined -> "0 passed"); it never changes it.
summary() {
  case "$1" in 0) echo "1 passed" ;; 1) echo "1 failed" ;; *) echo "0 passed" ;; esac
}
trap 'summary $?' EXIT

command -v claude >/dev/null 2>&1 || { echo "UNDETERMINED: claude not on PATH"; exit 126; }
plugin="$(ls -d "$HOME"/.claude/plugins/cache/yukineko/fugu-router/*/ 2>/dev/null | sort -V | tail -1)"
[ -n "$plugin" ] && [ -f "$plugin/hooks/hooks.json" ] || { echo "UNDETERMINED: installed fugu-router plugin not found"; exit 126; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/gh1345.XXXXXX")" || exit 126
cleanup() {
  rc=$?
  rm -f "$tmp/debug.log" "$tmp/stdout" "$tmp/stderr"
  rmdir "$tmp" 2>/dev/null
  summary "$rc"
}
trap cleanup EXIT

ahead() { git -C "$HOME/.fugu-router/sync" rev-list --count '@{u}..HEAD' 2>/dev/null || echo unknown; }
echo "plugin: $plugin"
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
