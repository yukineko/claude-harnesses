#!/usr/bin/env bash
# ============================================================================
# continuous-audit.sh — Continuous-Audit loop trigger scaffold (backlog 2630b4c5)
# ============================================================================
#
# The Continuous-Audit loop runs a PERIODIC adversarial review ROUND over a
# target set of crates (the fleet's gate crates by default): a finder proposes
# findings, a verifier CONFIRMS a subset, and each confirmed finding is turned
# into a regression test. Over successive rounds the per-round new-findings
# count should trend DOWN (convergence). See
# `docs/review-redesign-implementation-items.md` — 『継続運用の原則』.
#
# The finder/verifier review itself is LLM-driven and is NOT performed by this
# script. This script's DETERMINISTIC responsibility is to RECORD a round:
#   * for each CONFIRMED finding -> `overwatch record-finding ...` (the durable
#     findings-history/dedup ledger), then forward it to the backlog via
#     `overwatch review-queue --to-backlog` (idempotent on finding-id) so the
#     single actionable queue /flow drains never leaves an audit result
#     unattended, and
#   * once per round -> `overwatch audit-round record ...`
#     (the convergence-metrics ledger read back by `overwatch audit-metrics`).
# It then prints the finding list + `overwatch audit-metrics`.
#
# This is OPT-IN tooling. It is NOT wired into any always-on gate and does NOT
# change condukt/rollout behavior. Nothing is auto-installed. Everything is
# fail-soft (a store-write failure never breaks the loop — overwatch's
# never-break-a-turn invariant).
#
# --------------------------------------------------------------------------
# USAGE
#   scripts/continuous-audit.sh --help
#   scripts/continuous-audit.sh --dry-run                 # show plan, record nothing
#   scripts/continuous-audit.sh --round 2026W28           # default gate-crate targets
#   scripts/continuous-audit.sh --round 2026W28 --target specguard,stuckguard \
#       --new-findings 2 --confirmed 1 --regression-tests-added 1 \
#       --finding 'CA-2026W28-001|high|confirmed: unwrap in similarity path|crates/specguard/src/similarity.rs|because src/similarity.rs:42 unwraps an untrusted score without a length check'
#
# --finding may be repeated; format is: id|severity|summary|file|rationale
# (file and rationale optional). rationale is the verifier's stated reason for
# CONFIRMING the finding (e.g. a file:line-quoted argument) and is forwarded to
# `overwatch record-finding --rationale <value>` when non-empty. The legacy
# 4-field form (id|severity|summary|file) still works — rationale is simply
# empty in that case (backward compatible).
#
# VERDICTS ARE TRI-STATE (CONFIRMED / REFUTED / UNVERIFIED).
#   --finding             -> recorded with `--verdict confirmed`: established,
#                            bridged to the backlog as actionable work.
#   --unverified-finding  -> recorded with `--verdict unverified`: the verifier
#                            could NEITHER establish NOR refute it. Same format.
#                            It is NOT dropped (it stays on the review-queue
#                            surface, marked [UNVERIFIED]) and NOT bridged to the
#                            backlog — it stays pending re-verification.
#   REFUTED findings are not passed to this script at all, and claiming REFUTED
#   requires the verifier to have enumerated EVERY consumption path with
#   verbatim quotes (see the continuous-audit SKILL.md). "I could not find a
#   permissive path" is UNVERIFIED, not REFUTED — collapsing the two is the
#   same fail-open this loop audits other crates for.
#   --unverified <N> records the round's undetermined count in the ledger, kept
#   separate from --confirmed so `new - confirmed` is never read as "refuted".
# The COUNTS (--new-findings/--confirmed/--regression-tests-added) are recorded
# verbatim into the round ledger; the --finding entries are the CONFIRMED subset
# to ingest into the review queue. (They are independent inputs — the human/LLM
# review that produced them is upstream; this script only records.)
#
# --finder-model / --verifier-model (optional) record which model each stage
# used. When BOTH are given and are the SAME model, overwatch DETERMINISTICALLY
# enforces the finder!=verifier MUST (model diversity): a high-severity warning
# finding is recorded into the review queue (fail-soft — the round is still
# recorded and the loop is never broken). Omit both for the original behavior.
#
# --------------------------------------------------------------------------
# OPT-IN AUTOMATION TEMPLATES (nothing below is installed by this script)
#
# cron (see also scripts/continuous-audit.cron.example):
#   # Weekly adversarial audit round over the gate crates, Mondays 04:00.
#   # 0 4 * * 1 cd /path/to/claude-harnesses && scripts/continuous-audit.sh --round "$(date +\%Y\%W)" >> ~/.overwatch/continuous-audit.log 2>&1
#
# git pre-push hook (.git/hooks/pre-push) — trigger a round when gate-crate
# paths changed since the last push (opt-in; copy in manually):
#   #!/usr/bin/env bash
#   changed=$(git diff --name-only @{push}..HEAD 2>/dev/null | grep -E '^crates/(blastguard|propguard|specguard|stuckguard|mutategate|overwatch)/' || true)
#   if [ -n "$changed" ]; then
#     echo "gate-crate changes detected — consider running scripts/continuous-audit.sh --dry-run" >&2
#   fi
#   exit 0   # advisory only — never blocks the push (fail-soft)
# ============================================================================
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"

# Default target set = the GLOSSARY gate crates (kept in sync with
# scripts/rollout-plugins.sh GATE_CRATES, which also includes `overwatch`:
# it's the binary this loop itself calls to record findings/rounds, and the
# canary health-gate depends on it too, so it gets the same audit coverage
# as the crates it protects) PLUS `backlog`, an audit-only addition: backlog
# tasks pile up and rot if nothing reviews the crate that manages them, but
# backlog itself gates nothing, so it is NOT a GATE crate (no canary
# requirement on rollout, not in .githooks/pre-push's GATE_PATTERN). This is
# a strict superset of GATE_CRATES — see scripts/check-gate-crates-sync.py.
DEFAULT_TARGETS="blastguard,propguard,specguard,stuckguard,taintguard,mutategate,overwatch,parallelguard,backlog"

ROUND=""
TARGET="$DEFAULT_TARGETS"
NEW_FINDINGS=0
CONFIRMED=0
UNVERIFIED=0
REGRESSION_TESTS_ADDED=0
FINDER_MODEL=""
VERIFIER_MODEL=""
DRY_RUN=0
declare -a FINDINGS=()
# UNVERIFIED findings: undetermined verdicts. Kept in a SEPARATE array (not
# folded into FINDINGS) because they take a different path: recorded with
# `--verdict unverified`, visible in review-queue, NOT bridged to the backlog.
declare -a UNVERIFIED_FINDINGS=()

usage() {
  sed -n '2,83p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) usage 0 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --round) ROUND="${2:-}"; shift 2 ;;
    --target) TARGET="${2:-}"; shift 2 ;;
    --new-findings) NEW_FINDINGS="${2:-0}"; shift 2 ;;
    --confirmed) CONFIRMED="${2:-0}"; shift 2 ;;
    --unverified) UNVERIFIED="${2:-0}"; shift 2 ;;
    --regression-tests-added) REGRESSION_TESTS_ADDED="${2:-0}"; shift 2 ;;
    --finder-model) FINDER_MODEL="${2:-}"; shift 2 ;;
    --verifier-model) VERIFIER_MODEL="${2:-}"; shift 2 ;;
    --finding) FINDINGS+=("${2:-}"); shift 2 ;;
    --unverified-finding) UNVERIFIED_FINDINGS+=("${2:-}"); shift 2 ;;
    *) echo "unknown arg: $1" >&2; usage 2 ;;
  esac
done

# --- locate the overwatch binary (deterministic ledger core) -----------------
OW=""
if [ -n "${OVERWATCH_BIN:-}" ] && [ -x "${OVERWATCH_BIN}" ]; then
  OW="$OVERWATCH_BIN"
elif command -v overwatch >/dev/null 2>&1; then
  OW="$(command -v overwatch)"
elif [ -x "$REPO/target/release/overwatch" ]; then
  OW="$REPO/target/release/overwatch"
elif [ -x "$REPO/target/debug/overwatch" ]; then
  OW="$REPO/target/debug/overwatch"
else
  echo "continuous-audit: overwatch binary not found (build with: cargo build -p overwatch)" >&2
  echo "continuous-audit: nothing recorded (fail-soft)" >&2
  exit 0   # fail-soft: never hard-fail the caller
fi

# --- locate the backlog binary (findings sink) -------------------------------
# The CONFIRMED findings are forwarded to the backlog (the single actionable
# queue /flow drains) so an audit result is never left unattended. Resolve it
# the same way overwatch does and export OVERWATCH_BACKLOG_BIN so the bridge
# finds it even when `backlog` is not on PATH. Fail-soft: if none resolves the
# bridge itself warns and skips (never aborts the loop).
if [ -z "${OVERWATCH_BACKLOG_BIN:-}" ]; then
  if command -v backlog >/dev/null 2>&1; then
    OVERWATCH_BACKLOG_BIN="$(command -v backlog)"
  elif [ -x "$REPO/target/release/backlog" ]; then
    OVERWATCH_BACKLOG_BIN="$REPO/target/release/backlog"
  elif [ -x "$REPO/target/debug/backlog" ]; then
    OVERWATCH_BACKLOG_BIN="$REPO/target/debug/backlog"
  fi
fi
export OVERWATCH_BACKLOG_BIN

if [ -z "$ROUND" ]; then
  echo "continuous-audit: --round <round-id> is required (or --dry-run to preview)" >&2
  [ "$DRY_RUN" -eq 1 ] || exit 0
  ROUND="(dry-run)"
fi

echo "=== Continuous-Audit round ${ROUND} ==="
echo "  targets: ${TARGET}"
echo "  overwatch: ${OW}"
echo
echo "  NOTE: the finder->verifier adversarial review (LLM-driven) is upstream of"
echo "  this script. Reference the review tooling (overwatch review-queue for the"
echo "  standing surface; the re-review round in commit 38f613c is the POC of"
echo "  turning CONFIRMED findings into ignored regression tests). This script's"
echo "  deterministic job is only to RECORD the round below."
echo

# --- print the CONFIRMED finding list ---------------------------------------
echo "--- confirmed findings this round (${#FINDINGS[@]}) ---"
if [ "${#FINDINGS[@]}" -eq 0 ]; then
  echo "  (none supplied via --finding)"
else
  for f in "${FINDINGS[@]}"; do
    echo "  * ${f}"
  done
fi
echo
echo "--- UNVERIFIED (undetermined) findings this round (${#UNVERIFIED_FINDINGS[@]}) ---"
echo "  (recorded and kept visible, but NOT bridged to the backlog: pending re-verification)"
if [ "${#UNVERIFIED_FINDINGS[@]}" -eq 0 ]; then
  echo "  (none supplied via --unverified-finding)"
else
  for f in "${UNVERIFIED_FINDINGS[@]}"; do
    echo "  * ${f}"
  done
fi
echo

# fail-soft wrapper: a REPORTING overwatch call must not abort the loop.
#
# Use this only for calls whose failure costs visibility (metrics, reconcile,
# bridging). It must NOT be used for a call that returns a verdict: it maps
# every non-zero exit to "continuing", so a gate invoked through it can refuse
# while the loop sails past and prints its PASS line. That is how the round
# acceptance check would have been born inert (5b33b4cd) -- the binary refusing
# and the script reporting success are not in conflict if nobody reads the
# status.
run_ow() {
  if ! "$OW" "$@"; then
    echo "continuous-audit: WARNING overwatch $* failed (continuing, fail-soft)" >&2
  fi
}

# verdict-bearing wrapper: a non-zero exit is the answer, so it stops the run.
run_ow_gate() {
  if ! "$OW" "$@"; then
    echo >&2
    echo "continuous-audit: overwatch $1 refused this round (exit non-zero)." >&2
    echo "  The round was NOT recorded. Nothing above this line was rolled back;" >&2
    echo "  re-run once the reported condition is fixed." >&2
    exit 1
  fi
}

if [ "$DRY_RUN" -eq 1 ]; then
  echo "--- DRY RUN: would record (nothing written) ---"
  # Guard the expansion: under bash 3.2 + `set -u`, "${FINDINGS[@]}" on an empty
  # array is an "unbound variable" error, so only iterate when non-empty.
  if [ "${#FINDINGS[@]}" -gt 0 ]; then
    for f in "${FINDINGS[@]}"; do
      echo "  overwatch record-finding --verdict confirmed   <- ${f}"
    done
  fi
  if [ "${#UNVERIFIED_FINDINGS[@]}" -gt 0 ]; then
    for f in "${UNVERIFIED_FINDINGS[@]}"; do
      echo "  overwatch record-finding --verdict unverified  <- ${f}"
    done
  fi
  echo "  overwatch audit-round record --round ${ROUND} --target ${TARGET} \\"
  echo "    --new-findings ${NEW_FINDINGS} --confirmed ${CONFIRMED} --unverified ${UNVERIFIED} --regression-tests-added ${REGRESSION_TESTS_ADDED}${FINDER_MODEL:+ --finder-model ${FINDER_MODEL}}${VERIFIER_MODEL:+ --verifier-model ${VERIFIER_MODEL}}"
  echo "  overwatch review-queue --to-backlog   # forward each CONFIRMED finding to the backlog (idempotent on finding-id)"
  echo
  echo "--- current metrics (read-only) ---"
  run_ow audit-metrics
  exit 0
fi

# --- record each CONFIRMED finding into the review-queue findings store ------
# Guard the expansion: under bash 3.2 + `set -u`, "${FINDINGS[@]}" on an empty
# array is an "unbound variable" error, so only iterate when non-empty.
record_findings_with_verdict() {
  # $1 = verdict token (confirmed|unverified), rest = 'id|sev|summary|file|rationale' entries
  local verdict="$1"; shift
  local f fid fsev fsummary ffile frationale
  for f in "$@"; do
    # format: id|severity|summary|file|rationale  (severity, file & rationale may be empty)
    IFS='|' read -r fid fsev fsummary ffile frationale <<<"$f"
    args=(record-finding --finding-id "${fid:-CA-${ROUND}}" --source "continuous-audit" --summary "${fsummary:-${verdict} finding}" --verdict "$verdict")
    [ -n "${fsev:-}" ] && args+=(--severity "$fsev")
    [ -n "${ffile:-}" ] && args+=(--file "$ffile")
    [ -n "${frationale:-}" ] && args+=(--rationale "$frationale")
    run_ow "${args[@]}"
  done
}

if [ "${#FINDINGS[@]}" -gt 0 ]; then
  record_findings_with_verdict confirmed "${FINDINGS[@]}"
fi

# --- record each UNVERIFIED finding (undetermined verdict) -------------------
# Recorded so the claim is NOT silently dropped, with `--verdict unverified` so
# it is never read as an established finding and is not bridged to the backlog
# as actionable work by `review-queue --to-backlog` below.
if [ "${#UNVERIFIED_FINDINGS[@]}" -gt 0 ]; then
  record_findings_with_verdict unverified "${UNVERIFIED_FINDINGS[@]}"
fi

# --- record the round metrics into the convergence ledger --------------------
# When both --finder-model and --verifier-model are supplied, they are forwarded
# so overwatch can DETERMINISTICALLY enforce the finder!=verifier MUST (a same-
# model pair records a high-severity warning finding; fail-soft, never aborts).
# Omit both to keep the original, unchecked behavior (backward compatible).
round_args=(audit-round record
  --round "$ROUND"
  --target "$TARGET"
  --new-findings "$NEW_FINDINGS"
  --confirmed "$CONFIRMED"
  --unverified "$UNVERIFIED"
  --regression-tests-added "$REGRESSION_TESTS_ADDED")
[ -n "$FINDER_MODEL" ] && round_args+=(--finder-model "$FINDER_MODEL")
[ -n "$VERIFIER_MODEL" ] && round_args+=(--verifier-model "$VERIFIER_MODEL")
# Gate, not report: `record` refuses a round whose CONFIRMED findings are not
# closed by regression tests, and that refusal has to stop the run.
run_ow_gate "${round_args[@]}"

# --- auto-reconcile findings whose fix already landed (fail-soft) -----------
# Closes the "fix commit landed, nobody ran record-disposition" gap that lets
# review-queue go stale (see the 2026-07-17 incident: 18 already-fixed
# findings sat "open" for weeks). Scans recent commit messages for
# `CA-<crate>-<NNN>` references and auto-confirms any match still undisposed.
echo
echo "--- auto-reconciling fixed findings against recent commits ---"
run_ow reconcile-fixed --last-n 200 --json

# --- forward CONFIRMED findings to the backlog (single actionable queue) ------
# This closes the discover->fix loop deterministically: every not-yet-bridged
# finding-id becomes a `backlog add` (idempotent via bridged_findings.jsonl,
# severity high->p0/med->p1/low->p2), so /flow can pick it up and it is never
# left unattended. Fail-soft: a missing findings store / absent backlog is
# warned and skipped inside the bridge (never aborts the loop).
echo
echo "--- forwarding CONFIRMED findings to backlog (UNVERIFIED ones stay pending) ---"
run_ow review-queue --to-backlog

echo
echo "--- convergence metrics after this round ---"
run_ow audit-metrics

echo
echo "PASS: round ${ROUND} recorded (findings -> backlog, metrics -> audit ledger)."
