#!/usr/bin/env bash
# Repro for GitHub issue 1343: SessionStart hooks that print additionalContext
# at TOP LEVEL ({"additionalContext": ...}) instead of inside
# hookSpecificOutput. Claude Code only delivers the nested form to the model.
#
# Step 1 (platform fact, measured with claude -p): a throwaway SessionStart
#   hook prints the top-level shape (MARK-A) and, as a control, another run
#   prints the nested shape (MARK-B). The model is asked to echo any MARK-
#   token. Observed 2026-10-25 on Claude Code 2.1.284: top-level -> NONE,
#   nested -> MARK-B.
# Step 2 (the defect): if the platform drops the top-level shape, any
#   SessionStart emitter still printing json!({ "additionalContext": ... })
#   in the cited crates loses its context.
#
# Exit codes:
#   0   no defect (top-level shape is delivered, or no emitter uses it)
#   1   defect present (top-level dropped AND emitters still use it)
#   126 cannot determine (claude missing, hook did not run, control not seen);
#       chosen in the range backlog classifies as non-behavioural, never 0.
# Cost: two `claude -p --model haiku` calls.
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

tmp="$(mktemp -d "${TMPDIR:-/tmp}/gh1343.XXXXXX")" || exit 126
cleanup() {
  rc=$?
  rm -f "$tmp/top.json" "$tmp/nested.json"
  rmdir "$tmp/ran_a" "$tmp/ran_b" "$tmp" 2>/dev/null
  summary "$rc"
}
trap cleanup EXIT

cat >"$tmp/top.json" <<EOF
{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"mkdir '$tmp/ran_a' 2>/dev/null; printf '%s' '{\"additionalContext\":\"MARK-A\"}'"}]}]}}
EOF
cat >"$tmp/nested.json" <<EOF
{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"mkdir '$tmp/ran_b' 2>/dev/null; printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"MARK-B\"}}'"}]}]}}
EOF

prompt='If your context contains a token starting with MARK-, print it verbatim; else print NONE'
cd "$tmp" || exit 126
out_a="$(claude -p "$prompt" --model haiku --setting-sources local --settings "$tmp/top.json" </dev/null 2>&1)"
out_b="$(claude -p "$prompt" --model haiku --setting-sources local --settings "$tmp/nested.json" </dev/null 2>&1)"
echo "claude --version: $(claude --version)"
echo "top-level run stdout: $out_a"
echo "nested run stdout:    $out_b"

[ -d "$tmp/ran_a" ] || { echo "UNDETERMINED: top-level hook did not run"; exit 126; }
[ -d "$tmp/ran_b" ] || { echo "UNDETERMINED: nested hook did not run"; exit 126; }
case "$out_b" in *MARK-B*) ;; *) echo "UNDETERMINED: control (nested MARK-B) not seen by the model"; exit 126 ;; esac
case "$out_a" in
  *MARK-A*) echo "OK: top-level SessionStart additionalContext IS delivered; no defect"; exit 0 ;;
esac
echo "platform: top-level SessionStart additionalContext is NOT delivered to the model"

hits="$(grep -nE 'json!\(\{[^}]*"additionalContext"' \
  "$root/crates/backlog/src/main.rs" \
  "$root/crates/daily/src/main.rs" \
  "$root/crates/harness-status/src/main.rs" \
  "$root/crates/hypothesis/src/main.rs" \
  "$root/crates/taskprog/src/main.rs")"
grep_rc=$?
if [ "$grep_rc" -gt 1 ]; then echo "UNDETERMINED: emitter scan failed (grep exit $grep_rc)"; exit 126; fi
if [ -n "$hits" ]; then
  echo "DEFECT: SessionStart emitters still print top-level additionalContext:"
  echo "$hits"
  exit 1
fi
echo "OK: no top-level additionalContext emitter left in the cited crates"
exit 0
