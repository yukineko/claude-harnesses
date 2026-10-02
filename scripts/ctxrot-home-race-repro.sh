#!/usr/bin/env bash
# Repro for the ctxrot HOME env race (backlog b71c72a7).
# usage: ctxrot-home-race-repro.sh [N=200]
# Prints `runs=N failed=K`; exit 1 if K>0, 0 if K==0, non-zero on build
# failure, 3 if any run did not execute all three race participants by name
# (mistyped filter guard).
set -u
N="${1:-200}"
. "$HOME/.cargo/env" 2>/dev/null || true
cd "$(dirname "$0")/.." || exit 2

out="$(cargo test -p ctxrot --no-run 2>&1)" || { echo "$out"; echo "build failed"; exit 2; }
B="$(printf '%s\n' "$out" | grep -o 'target/[^ )]*/deps/ctxrot-[0-9a-f]*' | tail -n 1)"
[ -n "$B" ] && [ -x "$B" ] || { echo "could not locate ctxrot test binary"; exit 2; }

failed=0
for i in $(seq 1 "$N"); do
  o="$("$B" emit_violation find_transcript --test-threads=4 2>&1)"
  rc=$?
  # Both sides of the race must have run: the HOME-locked emit test AND the
  # HOME-mutating find_transcript tests. A count floor alone is not enough —
  # `emit_violation` by itself already matches 2 tests, so a typo in the
  # find_transcript half would pass a `-lt 2` check with the racer absent.
  if ! printf '%s\n' "$o" | grep -q '^test .*emit_violation_records_a_ctxrot_event ' ||
     ! printf '%s\n' "$o" | grep -q '^test .*find_transcript_absent_projects_dir_returns_none ' ||
     ! printf '%s\n' "$o" | grep -q '^test .*find_transcript_unreadable_projects_dir_does_not_panic '; then
    echo "run $i: a race participant did not run; filter broken"
    exit 3
  fi
  [ "$rc" -eq 0 ] || failed=$((failed + 1))
done
echo "runs=$N failed=$failed"
[ "$failed" -eq 0 ]
