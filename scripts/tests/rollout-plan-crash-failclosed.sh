#!/usr/bin/env bash
# A crash inside rollout-plugins.sh's plan() (consumed via `done < <(plan)`,
# which discards the producer's exit status) must abort NON-ZERO and must never
# print "done." -- for both the dry-run and the real (non-dry-run) paths.
#
# HARD SAFETY: runs a scratch COPY of the script in a scratch git repo whose
# marketplace.json has a malformed `source` (a string, not an object), with
# CLAUDE_PLUGIN_CACHE / CLAUDE_PLUGIN_REGISTRY / HOME all under a mktemp -d.
# The real ~/.claude/plugins is never referenced.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rollout-plan-crash.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

S="$TMP/repo"
mkdir -p "$S/scripts" "$S/.claude-plugin" "$TMP/home" "$TMP/cache/yukineko"
cp "$REPO/scripts/rollout-plugins.sh" "$S/scripts/"
printf '%s\n' '{"name":"yukineko","plugins":[{"name":"crashme","version":"1.0.0","source":"crates/crashme"}]}' \
  | tee "$S/.claude-plugin/marketplace.json" >/dev/null
printf '%s\n' '{"version":1,"plugins":{}}' | tee "$TMP/installed_plugins.json" >/dev/null
(cd "$S" && git init -q && git add -A \
  && git -c user.email=t@t -c user.name=t commit -q -m scratch)

run_case() { # name, extra args...
  local name="$1"; shift
  local out rc=0
  out="$(cd "$S" && HOME="$TMP/home" \
    CLAUDE_PLUGIN_CACHE="$TMP/cache/yukineko" \
    CLAUDE_PLUGIN_REGISTRY="$TMP/installed_plugins.json" \
    bash scripts/rollout-plugins.sh --no-canary --no-rebuild --no-sync "$@" 2>&1)" || rc=$?
  echo "--- $name (rc=$rc)"; echo "$out" | sed 's/^/    /'
  grep -q "AttributeError" <<<"$out" || fail "$name: scratch did not actually crash plan() (test is not exercising the defect)"
  [ "$rc" -ne 0 ] || fail "$name: plan() crashed but exit code was 0"
  if grep -qx "done\." <<<"$out"; then fail "$name: printed 'done.' after plan() crashed"; fi
  pass "$name: plan() crash -> non-zero exit (rc=$rc), no 'done.'"
}

run_case "dry-run" --dry-run
run_case "real (sandboxed)"

echo "PASS: plan() crash fails closed on dry-run and real paths."
