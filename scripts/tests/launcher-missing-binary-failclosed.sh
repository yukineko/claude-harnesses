#!/usr/bin/env bash
# Regression test: every plugin launcher (crates/*/bin/<name>) must NOT answer
# "no problem" when its per-platform binary is missing.
#
# The bug (backlog 8aeb4cab). A launcher is a tiny sh dispatcher that execs
# `<name>-<os>-<arch>`. When that build is absent, most launchers used to print
# one line to stderr and `exit 0` for EVERY argument vector. For a hook, exit 0
# with empty stdout is byte-for-byte the plugin's own "examined it, nothing to
# say" answer; for a CLI verb read by exit code (`fugu-router route > f`,
# `ship check`, `compass nudge --json`, `gauge report`) exit 0 is the success
# code. Either way the consumer cannot tell "checked, fine" from "did not run"
# (CLAUDE.md §1: whether output carries a verdict is decided by how it is
# consumed; §3: cannot-determine must resolve to the restrictive side).
#
# What is asserted, per class:
#
#   1. Every hook command registered in crates/*/hooks/hooks.json is run with
#      its binary absent, with a realistic stdin payload for its event.
#      INVARIANT: it must not exit 0 with empty stdout (the silent "fine"),
#      unless it is listed in EXEMPT below with the consumer-based reason.
#      A SessionEnd hook has no stdout channel at all (Claude Code discards
#      SessionEnd JSON), so it must exit non-zero with a stderr line — the
#      only thing that reaches the user.
#      Any stdout that looks like JSON must parse, and a hookSpecificOutput
#      carrying additionalContext must name the event it was run for.
#   2. Every launcher, given a verb that is NOT a hook entrypoint (a CLI verb),
#      must exit non-zero: the absence of an answer is not the success code.
#   3. A handful of named verdict-by-exit-code CLI verbs are spot-checked.
#
# The launchers are copied into a scratch plugin root that contains no
# binaries, so no real build is touched or removed.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/launcher-failclosed.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

fails=0
fail() { echo "  FAIL: $*"; fails=$((fails + 1)); }
pass() { echo "  ok:   $*"; }

# Hook invocations that may exit 0 with empty stdout, with the reason (the
# downstream consumers of that silence). Key: "<crate>|<event>|<args>".
# Each reason is repeated verbatim in the launcher that implements it.
exempt_reason() {
  case "$1" in
    "autoflow|PreCompact|pre-compact"|"autoflow|UserPromptSubmit|prompt-submit")
      echo "writer/reader of the resume marker are the same missing binary; nothing downstream reads its absence" ;;
    "condukt|Stop|state record-run --all")
      echo "state gate / check-oracle / reconcile treat an unrecorded task as INCOMPLETE (fails closed downstream)" ;;
    "ctxrot|UserPromptSubmit|guard"|"ctxrot|PreCompact|rescue"|"ctxrot|SessionStart|restore"|"ctxrot|PreToolUse|handoff"|"ctxrot|PostToolUse|handoff-record")
      echo "observability-only per crates/ctxrot/bin/ctxrot; the verdict entrypoints (preguard/toolguard/stop/statusline) are not exempt" ;;
    "parallelguard|PostToolUse|release"|"parallelguard|SessionStart|reset"|"parallelguard|UserPromptSubmit|reset"|"parallelguard|Stop|reset")
      echo "counter bookkeeping; the only reader of the counter is acquire, which denies" ;;
    "session-insights|PostToolUse|record")
      echo "per-tool-call counter; its only readers are session-insights stop (surfaces a systemMessage every turn) and report (exits 127) — the same missing binary" ;;
    *) return 1 ;;
  esac
}

# --- stage an empty plugin root per crate (launchers only) ------------------
for bindir in "$REPO"/crates/*/bin; do
  crate="$(basename "$(dirname "$bindir")")"
  for f in "$bindir"/*; do
    name="$(basename "$f")"
    case "$name" in *-linux-*|*-darwin-*|*-windows-*|*.exe) continue ;; esac
    mkdir -p "$TMP/root/$crate/bin"
    cp "$f" "$TMP/root/$crate/bin/$name"
    chmod +x "$TMP/root/$crate/bin/$name"
  done
done

# run <crate> <launcher> <event-or-empty> <args...>  -> sets rc, out, err
run() {
  local crate="$1" launcher="$2" event="$3"
  shift 3
  local payload
  payload="$(printf '{"hook_event_name":"%s","session_id":"launcher-failclosed-test","transcript_path":"%s/none.jsonl","cwd":"%s","stop_hook_active":false,"tool_name":"Bash","tool_input":{"command":"true"}}' \
    "$event" "$TMP" "$TMP")"
  printf '%s' "$payload" \
    | env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u BLASTGUARD_ASK \
        CLAUDE_PLUGIN_ROOT="$TMP/root/$crate" \
        sh "$TMP/root/$crate/bin/$launcher" "$@" >"$TMP/out" 2>"$TMP/err"
  rc=$?
  out="$(cat "$TMP/out")"
  err="$(cat "$TMP/err")"
}

check_json() { # <label> <event>
  local label="$1" event="$2"
  case "$out" in
    \{*) ;;
    *) return 0 ;;
  esac
  local verdict
  verdict="$(printf '%s' "$out" | EVENT="$event" python3 -c '
import json, os, sys
raw = sys.stdin.read()
try:
    objs = [json.loads(l) for l in raw.splitlines() if l.strip()]
except Exception as e:
    print("unparseable JSON: %s" % e); sys.exit(0)
for o in objs:
    h = o.get("hookSpecificOutput")
    if isinstance(h, dict) and "additionalContext" in h and h.get("hookEventName") != os.environ["EVENT"]:
        print("additionalContext for %r emitted under hookEventName %r" % (os.environ["EVENT"], h.get("hookEventName"))); sys.exit(0)
print("ok")
' 2>&1)"
  [ "$verdict" = ok ] || fail "$label: $verdict"
}

# --- 1. every registered hook command --------------------------------------
echo "== registered hook commands (binary absent)"
python3 - "$REPO" >"$TMP/hooks.tsv" <<'PY' || { echo "FAIL: could not enumerate hooks.json"; exit 1; }
import glob, json, os, re, sys
repo = sys.argv[1]
for hj in sorted(glob.glob(os.path.join(repo, "crates", "*", "hooks", "hooks.json"))):
    crate = hj.split(os.sep)[-3]
    d = json.load(open(hj))
    for event, groups in d.get("hooks", {}).items():
        for g in groups:
            for h in g.get("hooks", []):
                cmd = h.get("command", "")
                m = re.match(r"\$\{CLAUDE_PLUGIN_ROOT\}/bin/(\S+)\s*(.*)$", cmd)
                if not m:
                    print("UNPARSED\t%s\t%s\t%s" % (crate, event, cmd)); continue
                launcher, rest = m.group(1), m.group(2)
                # strip shell redirection / `|| true` tails: args stop at the first shell operator.
                args = []
                for tok in rest.split():
                    if tok in ("||", "&&", "|", ";") or tok.startswith("2>") or tok.startswith(">"):
                        break
                    args.append(tok)
                print("%s\t%s\t%s\t%s" % (crate, event, launcher, " ".join(args)))
PY
[ -s "$TMP/hooks.tsv" ] || { echo "FAIL: no hook commands enumerated"; exit 1; }

hooks_seen=0
while IFS=$'\t' read -r crate event launcher args; do
  if [ "$crate" = UNPARSED ]; then
    fail "unparseable hook command in $event: $launcher $args"
    continue
  fi
  hooks_seen=$((hooks_seen + 1))
  label="$crate $event: $launcher${args:+ $args}"
  # shellcheck disable=SC2086
  run "$crate" "$launcher" "$event" $args
  key="$crate|$event|$args"
  if [ "$event" = SessionEnd ]; then
    if [ "$rc" -ne 0 ] && [ -n "$err" ]; then
      pass "$label -> rc=$rc + stderr (SessionEnd: stderr is the only channel)"
    else
      fail "$label -> rc=$rc stderr=$([ -n "$err" ] && echo yes || echo NO) — SessionEnd discards stdout; a missing binary must exit non-zero with a stderr line"
    fi
    continue
  fi
  if [ "$rc" -eq 0 ] && [ -z "$out" ]; then
    if reason="$(exempt_reason "$key")"; then
      if [ -n "$err" ]; then
        pass "$label -> silent exit 0 EXEMPT ($reason)"
      else
        fail "$label -> exempt, but not even a stderr line"
      fi
    else
      fail "$label -> exit 0 with EMPTY stdout: indistinguishable from the plugin's own 'nothing to report'"
    fi
    continue
  fi
  check_json "$label" "$event"
  pass "$label -> rc=$rc stdout=$(printf '%s' "$out" | head -c 70 | tr '\n' ' ')…"
done <"$TMP/hooks.tsv"
[ "$hooks_seen" -gt 20 ] || fail "only $hooks_seen hook commands enumerated — enumeration is broken"

# --- 2. every launcher, non-hook CLI verb -----------------------------------
echo "== CLI verb on every launcher (binary absent) must exit non-zero"
launchers_seen=0
for d in "$TMP"/root/*/bin; do
  crate="$(basename "$(dirname "$d")")"
  for f in "$d"/*; do
    launcher="$(basename "$f")"
    launchers_seen=$((launchers_seen + 1))
    run "$crate" "$launcher" "" launcher-failclosed-probe
    if [ "$rc" -ne 0 ]; then
      pass "$launcher launcher-failclosed-probe -> rc=$rc"
    elif [ "$launcher" = blastguard ] \
      && printf '%s' "$out" | grep -Eq '"permissionDecision":"(deny|ask)"'; then
      # blastguard's binary ignores unknown args and runs its PreToolUse
      # verdict path, so an unknown verb IS the hook entrypoint there; the
      # launcher answers it with deny/ask, the restrictive verdict.
      pass "$launcher launcher-failclosed-probe -> restrictive PreToolUse verdict (unknown args = verdict path)"
    else
      fail "$launcher launcher-failclosed-probe -> rc=0 (stdout='$(printf '%s' "$out" | head -c 60)'): a CLI verb that did not run answered with the success code"
    fi
  done
done
[ "$launchers_seen" -gt 30 ] || fail "only $launchers_seen launchers staged — staging is broken"

# --- 3. named verdict-by-exit-code verbs ------------------------------------
echo "== named verdict-by-exit-code CLI verbs"
spot() { # <crate> <launcher> <args...>
  local crate="$1" launcher="$2"
  shift 2
  run "$crate" "$launcher" "" "$@"
  if [ "$rc" -ne 0 ]; then
    pass "$launcher $* -> rc=$rc"
  else
    fail "$launcher $* -> rc=0: read by its caller as the success/permissive answer"
  fi
}
spot condukt condukt state autonomy-check
spot condukt condukt policy answer
spot compass compass nudge --json
spot fugu-router fugu-router route --file x.json
spot gauge gauge report
spot ship ship check
spot hypothesis hypothesis list
spot session-insights session-insights report
spot specguard specguard pending --json
spot context-governor context-governor rollup

echo
if [ "$fails" -ne 0 ]; then
  echo "RED: $fails launcher case(s) answer 'fine' when the binary is missing"
  exit 1
fi
echo "GREEN: no launcher answers 'fine' when its binary is missing"
