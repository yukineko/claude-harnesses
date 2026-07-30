#!/usr/bin/env bash
# cap-target-dir.sh — bound the size of the SHARED cargo target-dir.
#
# WHY THIS EXISTS
#   .cargo/config.toml (machine-local; `/.cargo/` is gitignored) redirects
#   `target-dir` to an ABSOLUTE path — /tmp/condukt-build — because the C: drive
#   this repo lives on is full. Because it is absolute, that ONE directory
#   accumulates artifacts from every build made from this tree, across every
#   branch it is ever checked out to, and nothing reclaims it. Measured
#   2026-07-30, before a manual `cargo clean`: 37.1 GiB / 262,954 files.
#
#   This is a DETERMINISTIC gate, not a judgement: measure, compare against a
#   threshold, `cargo clean` only when over it. Under the threshold it is silent.
#
# WHY IT RUNS *BEFORE* A BUILD, NOT AFTER
#   Cleaning AFTER a build throws away the artifacts that build just produced,
#   so the next build is a full rebuild anyway: the same total compile cost as
#   cleaning first, PLUS one extra build's worth of work thrown away. Running
#   BEFORE means only the build that finds the cap already exceeded pays for a
#   full rebuild, and every build after it is incremental again. Hence: before.
#   (If you are here wondering "why isn't this a post-build hook?" — that is why.)
#
# USAGE
#   scripts/cap-target-dir.sh          # call it immediately before cargo build/test
#
# ENV
#   CARGO_TARGET_CAP_MB   cap in MB. Default 20000 (= 20 GB). 0 disables the gate.
#
# THIS SCRIPT'S FAILURE IS NEVER A BUILD FAILURE
#   Anything it cannot determine — `cargo metadata` broken, target-dir absent,
#   `du` unhappy — exits 0 having done nothing (the underlying tool's own
#   diagnostic is left visible; only THIS script stays quiet). A disk-hygiene
#   helper must not be able to take a build down with it.
set -euo pipefail
cd "$(dirname "$0")/.."

cap="${CARGO_TARGET_CAP_MB:-20000}"

# A non-numeric value would make the `-gt` below fail and, with `set -e`, abort
# the caller. An unparseable cap is not a size anyone can enforce, so treat it
# the same as the explicit disable value rather than dying over it.
case "$cap" in
  '' | *[!0-9]*) exit 0 ;;
esac

# 0 = explicitly disabled.
if [ "$cap" -eq 0 ]; then
  exit 0
fi

# Resolve the target-dir the SAME WAY scripts/build-plugin-bin.sh and
# scripts/rebuild-plugins.sh do — ask cargo instead of assuming ./target — so
# the .cargo/config.toml override (and CARGO_TARGET_DIR) is honored and this
# gate can never measure/clean a different directory from the one cargo writes.
#
# Taken as an `if` condition because `set -e` aborts on a failing command
# substitution in a plain assignment, and "cargo is missing/broken" is exactly a
# case that must exit 0 quietly. (`local x=$(cmd)` is banned in this repo for
# the related reason that `local` swallows the command's exit status; the
# declaration and the assignment stay split here for the same discipline.)
#
# cargo's OWN stderr is deliberately left visible: `cargo metadata` is silent on
# success, so if it speaks, the toolchain or a manifest is broken — a diagnostic
# worth keeping.
#
# THE TWO FAILURE MODES ARE NOT THE SAME and must not collapse into one
# `|| exit 0` — the metadata command's EXIT STATUS is read on its own, before
# anything is parsed out of its output:
#
#   rc != 0  — cargo could not answer: we are outside a cargo workspace, or the
#              toolchain is broken. Do NOT fall back to $PWD/target. Nothing
#              proves that directory is cargo's, and `cargo clean` would fail
#              for the very same reason. Cannot-determine must not fire a
#              destructive operation, so: exit 0, clean nothing.
#   rc == 0, but the output carries no target_directory (unexpected schema) —
#              the workspace itself is real and cargo's documented default
#              applies, so fall back to $PWD/target and judge normally.
meta=""
if ! meta="$(cargo metadata --no-deps --format-version=1)"; then
  exit 0
fi
target_dir="$(printf '%s' "$meta" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
target_dir="${target_dir:-$PWD/target}"

# Nothing built yet — nothing to cap.
[ -d "$target_dir" ] || exit 0

# Same discipline as above: keep `du`'s rc, and keep its stderr. A partial walk
# (unreadable subtree) makes `du` exit non-zero while still PRINTING a number,
# and that number is an undercount — acting on it would either miss the cap or
# clean on a figure nobody can vouch for. Non-zero rc => do nothing at all, and
# let the reason reach the log rather than /dev/null.
du_out=""
du_rc=0
du_out="$(du -sm "$target_dir")" || du_rc=$?
if [ "$du_rc" -ne 0 ]; then
  exit 0
fi
# `du -sm` prints "<megabytes>\t<path>"; keep the leading digits.
mb="${du_out%%[!0-9]*}"
[ -n "$mb" ] || exit 0

if [ "$mb" -gt "$cap" ]; then
  echo "cap-target-dir: $target_dir is ${mb}MB, over the ${cap}MB cap (CARGO_TARGET_CAP_MB) — running cargo clean before this build" >&2
  # Even a failed clean is not a build failure; the build below can still run
  # against whatever survived.
  cargo clean || true
fi
