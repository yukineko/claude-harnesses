#!/usr/bin/env bash
# Repro for the ctxrot HOME env race (backlog b71c72a7).
# usage: ctxrot-home-race-repro.sh [N=200]
# Prints `runs=N failed=K`; exit 1 if K>0, 0 if K==0, non-zero on build
# failure or if a run executed fewer than 2 tests (mistyped filter guard).
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
  n="$(printf '%s\n' "$o" | sed -n 's/^running \([0-9]*\) test.*/\1/p' | head -n 1)"
  if [ -z "$n" ] || [ "$n" -lt 2 ]; then
    echo "run $i: fewer than 2 tests ran (n=${n:-none}); filter broken"
    exit 3
  fi
  [ "$rc" -eq 0 ] || failed=$((failed + 1))
done
echo "runs=$N failed=$failed"
[ "$failed" -eq 0 ]
