#!/usr/bin/env bash
# Repro for GitHub issue 1344: Stop hooks (budgetguard gate.rs, taskprog
# update.rs) print a top-level {"additionalContext": ...}, a field Stop does
# not document. Observed 2026-10-25 on Claude Code 2.1.284 with claude -p and
# --output-format stream-json --verbose: the hook ran, the session ended after
# 1 turn (no continuation), and MARK-C appeared NOWHERE in the stream (not to
# the model, not to the user). A control Stop hook printing
# {"systemMessage":"MARK-D"} WAS surfaced as a system/informational event
# "Stop says: MARK-D".
#
# This script runs both Stop hooks (fire-once guards) in one claude -p call,
# then, if the top-level shape reaches nobody, scans the cited emitters.
#
# Exit codes:
#   0   no defect (top-level shape reaches someone, or no emitter uses it)
#   1   defect present
#   126 cannot determine (claude missing, hook did not run, control not seen)
# Cost: one `claude -p --model haiku` call.
set -u

# Final pytest/cargo-style summary line. It only reports the exit status
# the checks below chose (0 OK -> "1 passed", 1 DEFECT -> "1 failed",
# anything else, i.e. undetermined -> "0 passed"); it never changes it.
summary() {
  case "$1" in 0) echo "1 passed" ;; 1) echo "1 failed" ;; *) echo "0 passed" ;; esac
}
trap 'summary $?' EXIT

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)" || exit 126
command -v claude >/dev/null 2>&1 || { echo "UNDETERMINED: claude not on PATH"; exit 126; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/gh1344.XXXXXX")" || exit 126
cleanup() {
  rc=$?
  rm -f "$tmp/stop.json" "$tmp/out.jsonl"
  rmdir "$tmp/ran_c" "$tmp/ran_d" "$tmp" 2>/dev/null
  summary "$rc"
}
trap cleanup EXIT

cat >"$tmp/stop.json" <<EOF
{"hooks":{"Stop":[{"hooks":[
 {"type":"command","command":"cat >/dev/null; mkdir '$tmp/ran_c' 2>/dev/null || exit 0; printf '%s' '{\"additionalContext\":\"MARK-C\"}'"},
 {"type":"command","command":"cat >/dev/null; mkdir '$tmp/ran_d' 2>/dev/null || exit 0; printf '%s' '{\"systemMessage\":\"MARK-D\"}'"}
]}]}}
EOF

cd "$tmp" || exit 126
claude -p 'Reply with the single word READY. If your context contains a token starting with MARK-, also print it verbatim.' \
  --model haiku --setting-sources local --settings "$tmp/stop.json" \
  --output-format stream-json --verbose </dev/null >"$tmp/out.jsonl" 2>&1
echo "claude --version: $(claude --version)"
echo "stream lines mentioning MARK-:"
grep 'MARK-' "$tmp/out.jsonl"
echo "result line:"
grep '"type":"result"' "$tmp/out.jsonl" | cut -c1-400

[ -d "$tmp/ran_c" ] || { echo "UNDETERMINED: additionalContext Stop hook did not run"; exit 126; }
[ -d "$tmp/ran_d" ] || { echo "UNDETERMINED: systemMessage control Stop hook did not run"; exit 126; }
grep -q 'MARK-D' "$tmp/out.jsonl" || { echo "UNDETERMINED: control systemMessage MARK-D not surfaced"; exit 126; }
if grep -q 'MARK-C' "$tmp/out.jsonl"; then
  echo "OK: Stop top-level additionalContext reaches the session output; no defect"
  exit 0
fi
echo "platform: Stop top-level additionalContext reaches neither the model nor the output"

hits="$(grep -nE 'json!\(\{[^}]*"additionalContext"' \
  "$root/crates/budgetguard/src/gate.rs")"
grep_rc=$?
if [ "$grep_rc" -gt 1 ]; then echo "UNDETERMINED: emitter scan failed (grep exit $grep_rc)"; exit 126; fi
if [ -n "$hits" ]; then
  echo "DEFECT: Stop emitters still print top-level additionalContext:"
  echo "$hits"
  exit 1
fi
echo "OK: no top-level additionalContext Stop emitter left"
exit 0
