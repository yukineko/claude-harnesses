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
#   * for each CONFIRMED finding -> `overwatch record-finding ...`
#     (so it surfaces in `overwatch review-queue`), and
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
#       --finding 'CA-2026W28-001|high|confirmed: unwrap in similarity path|crates/specguard/src/similarity.rs'
#
# --finding may be repeated; format is: id|severity|summary|file (file optional).
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
#   changed=$(git diff --name-only @{push}..HEAD 2>/dev/null | grep -E '^crates/(blastguard|propguard|specguard|stuckguard|mutategate)/' || true)
#   if [ -n "$changed" ]; then
#     echo "gate-crate changes detected — consider running scripts/continuous-audit.sh --dry-run" >&2
#   fi
#   exit 0   # advisory only — never blocks the push (fail-soft)
# ============================================================================
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"

# Default target set = the GLOSSARY gate crates (kept in sync with
# scripts/rollout-plugins.sh GATE_CRATES).
DEFAULT_TARGETS="blastguard,propguard,specguard,stuckguard,mutategate"

ROUND=""
TARGET="$DEFAULT_TARGETS"
NEW_FINDINGS=0
CONFIRMED=0
REGRESSION_TESTS_ADDED=0
FINDER_MODEL=""
VERIFIER_MODEL=""
DRY_RUN=0
declare -a FINDINGS=()

usage() {
  sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
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
    --regression-tests-added) REGRESSION_TESTS_ADDED="${2:-0}"; shift 2 ;;
    --finder-model) FINDER_MODEL="${2:-}"; shift 2 ;;
    --verifier-model) VERIFIER_MODEL="${2:-}"; shift 2 ;;
    --finding) FINDINGS+=("${2:-}"); shift 2 ;;
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

# fail-soft wrapper: an overwatch call must never abort the loop.
run_ow() {
  if ! "$OW" "$@"; then
    echo "continuous-audit: WARNING overwatch $* failed (continuing, fail-soft)" >&2
  fi
}

if [ "$DRY_RUN" -eq 1 ]; then
  echo "--- DRY RUN: would record (nothing written) ---"
  # Guard the expansion: under bash 3.2 + `set -u`, "${FINDINGS[@]}" on an empty
  # array is an "unbound variable" error, so only iterate when non-empty.
  if [ "${#FINDINGS[@]}" -gt 0 ]; then
    for f in "${FINDINGS[@]}"; do
      echo "  overwatch record-finding  <- ${f}"
    done
  fi
  echo "  overwatch audit-round record --round ${ROUND} --target ${TARGET} \\"
  echo "    --new-findings ${NEW_FINDINGS} --confirmed ${CONFIRMED} --regression-tests-added ${REGRESSION_TESTS_ADDED}${FINDER_MODEL:+ --finder-model ${FINDER_MODEL}}${VERIFIER_MODEL:+ --verifier-model ${VERIFIER_MODEL}}"
  echo
  echo "--- current metrics (read-only) ---"
  run_ow audit-metrics
  exit 0
fi

# --- record each CONFIRMED finding into the review-queue findings store ------
# Guard the expansion: under bash 3.2 + `set -u`, "${FINDINGS[@]}" on an empty
# array is an "unbound variable" error, so only iterate when non-empty.
if [ "${#FINDINGS[@]}" -gt 0 ]; then
  for f in "${FINDINGS[@]}"; do
    # format: id|severity|summary|file  (severity & file may be empty)
    IFS='|' read -r fid fsev fsummary ffile <<<"$f"
    args=(record-finding --finding-id "${fid:-CA-${ROUND}}" --source "continuous-audit" --summary "${fsummary:-confirmed finding}")
    [ -n "${fsev:-}" ] && args+=(--severity "$fsev")
    [ -n "${ffile:-}" ] && args+=(--file "$ffile")
    run_ow "${args[@]}"
  done
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
  --regression-tests-added "$REGRESSION_TESTS_ADDED")
[ -n "$FINDER_MODEL" ] && round_args+=(--finder-model "$FINDER_MODEL")
[ -n "$VERIFIER_MODEL" ] && round_args+=(--verifier-model "$VERIFIER_MODEL")
run_ow "${round_args[@]}"

echo
echo "--- convergence metrics after this round ---"
run_ow audit-metrics

echo
echo "PASS: round ${ROUND} recorded (findings -> review-queue, metrics -> audit ledger)."
