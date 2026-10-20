#!/usr/bin/env bash
# Independent behavioural test for scripts/check-closure-evidence.py (pre-commit
# closure-evidence gate). Spec: a terminal transition (done / cancelled) needs an
# EXECUTED, COMMITTED test (or doc-only / duplicate); a cancel needs a discard;
# a human ruling is never evidence (2026-10-15). The gate compares HEAD vs INDEX
# for .backlog/tasks.toml + tasks.done.toml and RE-RUNS the recorded tests.
#
# Every case builds a throwaway git repo. Block cases are single-variable
# MUTATIONS of a baseline that is itself asserted to PASS (case P1), so a block
# can only come from the mutated field, not from a broken fixture.
#
# Verdict contract asserted here:
#   block -> exit status EXACTLY 1 and the gate's output mentions "closure"
#            (a missing script exits 2 from python3, so it can never be read as a
#            block; a crash exits 1 with a Traceback, which we also reject)
#   pass  -> exit status EXACTLY 0
#
# ASSUMED closure-table shape (spec left the TOML keys to the implementer; this
# test pins them):  [task.closure] reason, duplicate_of, doc_only_commit;
# [task.closure.green] runner cmd exit passed rev observed_at output_digest excerpt;
# [task.closure.red] rev exit kind; discard closure: [task.closure]
# reason = "discard" + discard_reason (cancelled only; never justifies done).
#
# Exit 0 iff every expectation holds; otherwise non-zero naming each failed case.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
GATE="$REPO/scripts/check-closure-evidence.py"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/closure-evidence-gate.XXXXXX")" || { echo "FAIL: mktemp root" >&2; exit 1; }
[ -d "$TMP" ] || { echo "FAIL: temp root not a directory" >&2; exit 1; }
trap 'rm -rf "$TMP"' EXIT

FAILED=()
NCASES=0
ZERO40=0000000000000000000000000000000000000000

# ---------------------------------------------------------------- fixtures
reset_vars() {
  unset DISCARD_REASON G_RUNNER G_CMD G_EXIT G_PASSED G_REV R_REV R_EXIT R_KIND R_OVERLAY NO_RED NO_GREEN \
        FORGED_RED DOC_FILE DOC_COMMIT_SRC R_REV_NONANC NO_FEATURE DUP_OF STATUS RK_BY RK_AT RK_VIA NO_TEST_STAGE
}

new_repo() { # sets D, BASE
  D="$(mktemp -d "$TMP/case.XXXXXX")" || { echo "FAIL: mktemp case dir" >&2; exit 1; }
  [ -d "$D" ] || { echo "FAIL: case dir missing" >&2; exit 1; }
  ( cd "$D" && git init -q -b main && git config user.email t@t.t && git config user.name t \
    && git config commit.gpgsign false ) || { echo "FAIL: git init" >&2; exit 1; }
  mkdir -p "$D/.backlog" "$D/tests" "$D/docs"
  cat >"$D/.backlog/tasks.toml" <<'T'
[[task]]
id = "aaaa1111"
title = "target"
status = "pending"
T
  cat >"$D/.backlog/tasks.done.toml" <<'T'
[[task]]
id = "legacy01"
title = "old closed row, no closure table"
status = "done"
T
  echo readme >"$D/README.md"
  ( cd "$D" && git add -A && git commit -q -m base ) || { echo "FAIL: base commit" >&2; exit 1; }
  BASE="$(git -C "$D" rev-parse HEAD)"
}

write_test_script() { # tests/t.sh passes iff feature.txt exists
  cat >"$D/tests/t.sh" <<'T'
#!/usr/bin/env bash
if [ -f feature.txt ]; then echo "test result: ok. 1 passed; 0 failed"; exit 0; fi
echo "test result: FAILED. 0 passed; 1 failed"; exit 1
T
  chmod +x "$D/tests/t.sh"
}

# emit the [task.closure*] tables for an F2P closure from G_*/R_* vars
closure_f2p() { # $1 = green rev default, $2 = red rev default
  echo '[task.closure]'
  echo 'reason = "fixed-f2p"'
  if [ -z "${NO_GREEN:-}" ]; then
    echo '[task.closure.green]'
    echo "runner = \"${G_RUNNER:-bash}\""
    echo "cmd = \"${G_CMD:-bash tests/t.sh}\""
    echo "exit = ${G_EXIT:-0}"
    echo "passed = ${G_PASSED:-1}"
    echo "rev = \"${G_REV:-$1}\""
    echo 'observed_at = 1790000000'
    echo 'output_digest = "sha256:0000"'
    echo 'excerpt = "test result: ok. 1 passed; 0 failed"'
  fi
  if [ -z "${NO_RED:-}" ]; then
    echo '[task.closure.red]'
    echo "rev = \"${R_REV:-$2}\""
    echo "exit = ${R_EXIT:-1}"
    echo "kind = \"${R_KIND:-behavioural}\""
    [ -z "${R_OVERLAY:-}" ] || echo "overlay = ${R_OVERLAY}"
  fi
}

# move aaaa1111 from pending file to done file with given closure text on stdin
stage_done_row() { # $1 = status, stdin = closure tables
  local status="$1" closure; closure="$(cat)"
  printf '' >"$D/.backlog/tasks.toml"
  {
    cat <<'T'
[[task]]
id = "legacy01"
title = "old closed row, no closure table"
status = "done"

T
    printf '[[task]]\nid = "aaaa1111"\ntitle = "target"\nstatus = "%s"\n%s\n' "$status" "$closure"
  } >"$D/.backlog/tasks.done.toml"
}

# Stage a full F2P closure. Honours vars (see reset_vars).
stage_f2p() {
  new_repo
  write_test_script
  local head="$BASE" red="$BASE"
  if [ -n "${FORGED_RED:-}" ]; then # feature already present at the claimed red rev
    echo done >"$D/feature.txt"
    ( cd "$D" && git add feature.txt && git commit -q -m "feature already there" ) || exit 1
    head="$(git -C "$D" rev-parse HEAD)"; red="$head"
  fi
  if [ -n "${R_REV_NONANC:-}" ]; then
    ( cd "$D" && git checkout -q -b side "$BASE" && echo s >side.txt && git add side.txt \
      && git commit -q -m side && git checkout -q main ) || exit 1
    red="$(git -C "$D" rev-parse side)"
  fi
  [ -n "${FORGED_RED:-}" ] || { [ -n "${NO_FEATURE:-}" ] || echo done >"$D/feature.txt"; }
  closure_f2p "$head" "$red" | stage_done_row "${STATUS:-done}"
  local paths=".backlog/tasks.toml .backlog/tasks.done.toml"
  [ -n "${NO_TEST_STAGE:-}" ] || paths="$paths tests/t.sh"
  [ -f "$D/feature.txt" ] && [ -z "${FORGED_RED:-}" ] && paths="$paths feature.txt"
  ( cd "$D" && git add $paths ) || exit 1
}

# The fix lives in a TEST file (tests/subject.txt, buggy at the red rev,
# fixed in the index); the repro tests/t.sh passes iff it reads `fixed`.
# A script runner's default overlay is the script alone, so the red tree keeps
# the buggy subject; R_OVERLAY pins the closure.red.overlay shape checks
# (narrowed overlay accepted, widened / empty / non-list blocked).
stage_fix_in_test_file() {
  new_repo
  echo buggy >"$D/tests/subject.txt"
  ( cd "$D" && git add tests/subject.txt && git commit -q -m "subject test (buggy)" ) || exit 1
  local red; red="$(git -C "$D" rev-parse HEAD)"
  echo fixed >"$D/tests/subject.txt"
  cat >"$D/tests/t.sh" <<'T'
#!/usr/bin/env bash
if grep -q '^fixed$' tests/subject.txt; then echo "test result: ok. 1 passed; 0 failed"; exit 0; fi
echo "test result: FAILED. 0 passed; 1 failed"; exit 1
T
  chmod +x "$D/tests/t.sh"
  closure_f2p "$red" "$red" | stage_done_row done
  ( cd "$D" && git add .backlog/tasks.toml .backlog/tasks.done.toml tests/t.sh tests/subject.txt ) || exit 1
}

stage_doc_only() { # DOC_FILE (default docs/x.md)
  new_repo
  local f="${DOC_FILE:-docs/x.md}"
  mkdir -p "$D/$(dirname "$f")"; echo "change" >"$D/$f"
  ( cd "$D" && git add "$f" && git commit -q -m "doc change" ) || exit 1
  local c; c="$(git -C "$D" rev-parse HEAD)"
  printf '[task.closure]\nreason = "doc-only"\ndoc_only_commit = "%s"\n' "$c" | stage_done_row done
  ( cd "$D" && git add .backlog/tasks.toml .backlog/tasks.done.toml ) || exit 1
}

stage_ruling() { # STATUS default cancelled; RK_* overrides
  new_repo
  {
    echo '[task.closure]'
    echo 'reason = "ruling"'
    echo '[task.closure.ruling]'
    echo 'kind = "judgment"'
    echo 'rationale = "value judgment"'
    [ -n "${RK_BY-x}" ] && [ "${RK_BY-x}" != "DROP" ] && echo "approved_by = \"${RK_BY-yuki}\""
    [ "${RK_AT-x}" != "DROP" ] && echo "approved_at = ${RK_AT-1790000000}"
    [ "${RK_VIA-x}" != "DROP" ] && echo "approved_via = \"${RK_VIA-tty}\""
  } | stage_done_row "${STATUS:-cancelled}"
  ( cd "$D" && git add .backlog/tasks.toml .backlog/tasks.done.toml ) || exit 1
}

stage_discard() { # STATUS default cancelled; DISCARD_REASON unset -> default text, DROP -> omit, else literal
  new_repo
  {
    echo '[task.closure]'
    echo 'reason = "discard"'
    case "${DISCARD_REASON-unset}" in
      DROP) ;;
      unset) echo 'discard_reason = "unproven speculation"' ;;
      *) echo "discard_reason = \"${DISCARD_REASON}\"" ;;
    esac
  } | stage_done_row "${STATUS:-cancelled}"
  ( cd "$D" && git add .backlog/tasks.toml .backlog/tasks.done.toml ) || exit 1
}

# ---------------------------------------------------------------- runner
run_gate() { # sets RC OUT
  OUT="$( cd "$D" && BACKLOG_TEST_TIMEOUT_SECS=60 python3 "$GATE" 2>&1 )"; RC=$?
}

expect() { # $1 name, $2 block|pass
  local name="$1" want="$2" why=""
  NCASES=$((NCASES+1))
  run_gate
  if [ "$want" = pass ]; then
    [ "$RC" -eq 0 ] || why="expected exit 0, got $RC"
  else
    if [ "$RC" -ne 1 ]; then why="expected block (exit 1), got $RC"
    elif ! printf '%s' "$OUT" | grep -qi 'closure'; then why="exit 1 but output does not name the closure gate"
    elif printf '%s' "$OUT" | grep -q 'Traceback'; then why="exit 1 is a crash (Traceback), not a verdict"
    fi
  fi
  if [ -n "$why" ]; then
    FAILED+=("$name: $why")
    echo "  FAIL [$want] $name: $why" >&2
    printf '%s\n' "$OUT" | head -5 | sed 's/^/        | /' >&2
  else
    echo "  ok   [$want] $name"
  fi
}

case_() { reset_vars; }

# ---------------------------------------------------------------- PASS cases
case_; stage_f2p; expect "P1 valid F2P (fails at red rev, passes at staged tree)" pass
case_; stage_doc_only; expect "P2 valid doc-only (docs/x.md only)" pass
case_; stage_doc_only; expect "P3 doc-only README" pass
case_; new_repo
  printf '[[task]]\nid = "bbbb2222"\ntitle = "new"\nstatus = "pending"\n' >>"$D/.backlog/tasks.toml"
  ( cd "$D" && git add .backlog/tasks.toml ); expect "P4 legacy terminal row unchanged, new pending row added" pass
case_; new_repo
  printf '[[task]]\nid = "cccc3333"\ntitle = "judge"\nstatus = "needs-ruling"\n[task.closure.ruling]\nkind = "judgment"\nrationale = "value"\n' >>"$D/.backlog/tasks.toml"
  ( cd "$D" && git add .backlog/tasks.toml ); expect "B31 needs-ruling status row is no longer a valid status (2026-10-15 ruling)" block
case_; new_repo; echo x >"$D/other.txt"; ( cd "$D" && git add other.txt ); expect "P6 commit not touching .backlog" pass
case_; STATUS=cancelled; stage_ruling; expect "B32 cancelled with full TTY ruling record (ruling is no longer an evidence class)" block
case_; STATUS=done; stage_ruling; expect "B33 done with full TTY ruling record and no other evidence" block
case_; new_repo
  printf '[[task]]\nid = "dddd4444"\ntitle = "unverified"\nstatus = "unconfirmed"\n' >>"$D/.backlog/tasks.toml"
  ( cd "$D" && git add .backlog/tasks.toml ); expect "B34 unconfirmed status row is no longer a valid status (2026-10-15 ruling)" block
case_; new_repo
  { printf '[task.closure]\nreason = "duplicate"\nduplicate_of = "legacy01"\n' ; } | stage_done_row done
  ( cd "$D" && git add .backlog/tasks.toml .backlog/tasks.done.toml ); expect "P8 duplicate_of existing done row" pass
case_; stage_discard; expect "P9 cancelled with discard closure + non-empty discard_reason" pass
case_; DISCARD_REASON="no evidence, speculation only"; stage_discard; expect "P10 cancelled discard with a different non-empty reason" pass
case_; G_RUNNER=script; stage_f2p; expect "P11 CLI runner label script for bash tests/t.sh (backlog b0c3d263)" pass
case_; G_RUNNER=sh; G_CMD="sh tests/t.sh"; stage_f2p; expect "P12 legacy first-word runner sh for sh tests/t.sh" pass
case_; R_OVERLAY='["tests/t.sh"]'; stage_fix_in_test_file; expect "P13 fix in a test file, red.overlay = the repro only" pass
case_; R_OVERLAY='["tests/t.sh", "tests/subject.txt"]'; stage_fix_in_test_file; expect "B39 red.overlay widens the default overlay (copies the fixed subject in)" block
case_; R_OVERLAY='[]'; stage_fix_in_test_file; expect "B40 red.overlay is an empty list" block
case_; R_OVERLAY='"tests/t.sh"'; stage_fix_in_test_file; expect "B41 red.overlay is not a list" block

# ---------------------------------------------------------------- BLOCK: no / malformed evidence
case_; new_repo
  printf '' >"$D/.backlog/tasks.toml"
  printf '[[task]]\nid = "legacy01"\ntitle = "x"\nstatus = "done"\n\n[[task]]\nid = "aaaa1111"\ntitle = "target"\nstatus = "done"\n' >"$D/.backlog/tasks.done.toml"
  ( cd "$D" && git add -A ); expect "B01 newly done row (moved to done file) with no closure table" block
case_; new_repo
  sed 's/status = "pending"/status = "done"/' "$D/.backlog/tasks.toml" >"$D/t.new" && mv "$D/t.new" "$D/.backlog/tasks.toml"
  ( cd "$D" && git add -A ); expect "B02 status flipped to done in place, no closure table" block
case_; G_EXIT=1; stage_f2p; expect "B03 green block exit != 0" block
case_; G_PASSED=0; stage_f2p; expect "B04 green passed = 0" block
case_; G_RUNNER=grep; G_CMD="grep -q done feature.txt"; stage_f2p; expect "B05 runner outside allowlist (grep)" block
case_; G_RUNNER=cargo-test; stage_f2p; expect "B35 CLI label cargo-test on a bash script command (label/shape mismatch)" block
case_; G_RUNNER=pytest; stage_f2p; expect "B36 CLI label pytest on a bash script command (label/shape mismatch)" block
case_; G_RUNNER=python3; stage_f2p; expect "B37 legacy runner python3 on a bash script command (program mismatch)" block
case_; G_RUNNER=shell-script; stage_f2p; expect "B38 unknown runner label on an allowlisted command" block
case_; G_REV=abc123; stage_f2p; expect "B06 green rev not 40-hex" block
case_; G_REV=$ZERO40; stage_f2p; expect "B07 green rev is not a commit" block
case_; NO_RED=1; stage_f2p; expect "B08 F2P closure missing red block" block
case_; R_EXIT=0; stage_f2p; expect "B09 red block records exit 0" block
case_; R_REV_NONANC=1; stage_f2p; expect "B10 red rev exists but is not an ancestor of HEAD" block
case_; FORGED_RED=1; stage_f2p; expect "B11 forged red: recorded test actually PASSES at red rev (gate re-runs)" block
case_; NO_FEATURE=1; stage_f2p; expect "B12 forged green: recorded test actually FAILS at staged tree (gate re-runs)" block
case_; NO_TEST_STAGE=1; stage_f2p; expect "B13 test script path absent from the index" block
case_; new_repo
  { printf '[task.closure]\nreason = "duplicate"\nduplicate_of = "nonexist"\n'; } | stage_done_row done
  ( cd "$D" && git add .backlog/tasks.toml .backlog/tasks.done.toml ); expect "B14 duplicate_of absent id" block

# ---------------------------------------------------------------- BLOCK: doc-only predicate
case_; DOC_FILE=src/lib.rs; stage_doc_only; expect "B15 doc-only commit touches a .rs file" block
case_; DOC_FILE=skills/foo/SKILL.md; stage_doc_only; expect "B16 doc-only commit touches SKILL.md (prompt-bearing = code)" block
case_; DOC_FILE=agents/x.md; stage_doc_only; expect "B17 doc-only commit touches agents/x.md (prompt-bearing = code)" block

# ---------------------------------------------------------------- BLOCK: rulings
# A ruling is not an evidence class at all (B32/B33 pin the full record);
# B18-B22, which pinned the shape of an approval record, were removed with it.

# ---------------------------------------------------------------- BLOCK: parse / tamper
case_; new_repo
  printf 'this is [[[ not toml\n' >"$D/.backlog/tasks.toml"
  ( cd "$D" && git add -A ); expect "B23 unparseable staged TOML" block
case_; stage_f2p; ( cd "$D" && git commit -q -m closed ) || exit 1
  printf '[[task]]\nid = "legacy01"\ntitle = "x"\nstatus = "done"\n\n[[task]]\nid = "aaaa1111"\ntitle = "target"\nstatus = "done"\n' >"$D/.backlog/tasks.done.toml"
  ( cd "$D" && git add .backlog/tasks.done.toml ); expect "B24 terminal row's closure table removed in the index" block
case_; stage_f2p; ( cd "$D" && git commit -q -m closed ) || exit 1
  sed 's/^passed = 1/passed = 0/' "$D/.backlog/tasks.done.toml" >"$D/t.new" && mv "$D/t.new" "$D/.backlog/tasks.done.toml"
  ( cd "$D" && git add .backlog/tasks.done.toml ); expect "B25 terminal row's closure table altered in the index" block

# ---------------------------------------------------------------- BLOCK: discard
case_; DISCARD_REASON=DROP; stage_discard; expect "B26 cancelled discard row lacking discard_reason" block
case_; DISCARD_REASON=""; stage_discard; expect "B27 cancelled discard row with empty discard_reason" block
case_; DISCARD_REASON="   "; stage_discard; expect "B28 cancelled discard row with whitespace-only discard_reason" block
case_; STATUS=done; stage_discard; expect "B29 done row closed by a discard (discard never justifies done)" block
case_; new_repo
  printf '' >"$D/.backlog/tasks.toml"
  printf '[[task]]\nid = "legacy01"\ntitle = "x"\nstatus = "done"\n\n[[task]]\nid = "aaaa1111"\ntitle = "target"\nstatus = "cancelled"\n' >"$D/.backlog/tasks.done.toml"
  ( cd "$D" && git add -A ); expect "B30 cancelled row with no closure table" block

echo
if [ "${#FAILED[@]}" -ne 0 ]; then
  echo "closure-evidence-gate: ${#FAILED[@]} of $NCASES case(s) FAILED:" >&2
  for f in "${FAILED[@]}"; do echo "  - $f" >&2; done
  exit 1
fi
echo "closure-evidence-gate: all $NCASES cases ok"
