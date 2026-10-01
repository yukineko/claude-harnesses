#!/usr/bin/env bash
# Reproduce concurrent `cargo test` interference (backlog b71c72a7).
# Usage: scripts/concurrent-cargo-test.sh <crate> <test-filter> <iterations>
# Builds once, then per iteration runs two `cargo test -p <crate> <filter>`
# processes concurrently and checks EACH exit status separately.
# Exits non-zero if any process in any iteration failed (output is printed),
# or on bad args / missing cargo / build failure.
set -u

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <crate> <test-filter> <iterations>" >&2
  exit 2
fi
crate=$1
filter=$2
iters=$3
case "$iters" in
  ''|*[!0-9]*) echo "iterations must be a positive integer: $iters" >&2; exit 2 ;;
esac
if [ "$iters" -lt 1 ]; then
  echo "iterations must be >= 1" >&2
  exit 2
fi

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found" >&2
  exit 3
fi

if ! cargo test -p "$crate" --no-run; then
  echo "build failed for $crate" >&2
  exit 4
fi

tmp=$(mktemp -d) || { echo "mktemp failed" >&2; exit 5; }
trap 'rm -rf "$tmp"' EXIT

failed=0
for i in $(seq 1 "$iters"); do
  cargo test -p "$crate" "$filter" >"$tmp/a.out" 2>&1 &
  p1=$!
  cargo test -p "$crate" "$filter" >"$tmp/b.out" 2>&1 &
  p2=$!
  wait "$p1"; r1=$?
  wait "$p2"; r2=$?
  if [ "$r1" -ne 0 ] || [ "$r2" -ne 0 ]; then
    failed=$((failed + 1))
    echo "=== iteration $i FAILED (proc1 exit=$r1, proc2 exit=$r2) ==="
    if [ "$r1" -ne 0 ]; then echo "--- proc1 output ---"; cat "$tmp/a.out"; fi
    if [ "$r2" -ne 0 ]; then echo "--- proc2 output ---"; cat "$tmp/b.out"; fi
  fi
done

echo "iterations=$iters failed_iterations=$failed"
if [ "$failed" -ne 0 ]; then
  exit 1
fi
exit 0
