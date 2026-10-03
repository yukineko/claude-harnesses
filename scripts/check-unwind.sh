#!/usr/bin/env bash
# Lock the panic = "unwind" invariant at build time.
#
# Both panic barriers in harness-core rely on std::panic::catch_unwind, and they
# resolve a panic in opposite directions (backlog f6919056 corrected this prose):
#   * hook::run_hook — observability hooks only (no verdict): a panic is
#     swallowed and the hook exits 0.
#   * gate::run_guarded — Stop gates (they return a verdict): a panic FAILS
#     CLOSED, emitting {"decision":"block"} and surfacing the crash; only a
#     second consecutive crash (stop_hook_active) falls to a bounded allow.
#     See crates/harness-core/src/gate/run.rs.
# Under panic="abort" catch_unwind is a silent NO-OP: the process aborts before
# either barrier runs, so a crashed gate emits no decision at all and neither
# contract holds. Two independent guards must both hold; this script asserts
# both so neither can silently regress:
#   1. the workspace [profile.release] pins panic = "unwind" (and nothing pins
#      "abort"), so the distributed --release binaries are unwinding.
#   2. harness-core keeps the #[cfg(not(panic = "unwind"))] compile_error!
#      backstop, so any crate compiled under abort fails the build loudly.
#
# Exit 0 when both hold; exit 1 listing every violation otherwise.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cargo_toml="$root/Cargo.toml"
lib="$root/crates/harness-core/src/lib.rs"
fail=0

# 1a. [profile.release] must pin panic = "unwind". Parse that section only, so a
#     panic= line under another profile cannot satisfy the check.
if ! awk '
  /^\[profile\.release\]/ { in_rel = 1; next }
  /^\[/                    { in_rel = 0 }
  in_rel && /^[[:space:]]*panic[[:space:]]*=[[:space:]]*"unwind"/ { found = 1 }
  END { exit(found ? 0 : 1) }
' "$cargo_toml"; then
  echo "ERROR: [profile.release] must pin panic = \"unwind\" in Cargo.toml" >&2
  echo "       (catch_unwind is a NO-OP under panic=abort => breaks never-break-a-turn)" >&2
  fail=1
fi

# 1b. Reject an explicit panic = "abort" anywhere in the workspace manifest.
if grep -nE '^[[:space:]]*panic[[:space:]]*=[[:space:]]*"abort"' "$cargo_toml" >/dev/null; then
  echo "ERROR: panic = \"abort\" found in Cargo.toml — forbidden (defeats catch_unwind):" >&2
  grep -nE '^[[:space:]]*panic[[:space:]]*=[[:space:]]*"abort"' "$cargo_toml" >&2
  fail=1
fi

# 2. The compile_error! backstop must remain in harness-core.
if ! grep -q 'cfg(not(panic = "unwind"))' "$lib" || ! grep -q 'compile_error!' "$lib"; then
  echo "ERROR: harness-core must keep the #[cfg(not(panic = \"unwind\"))] compile_error! backstop" >&2
  echo "       (the per-crate guard that fails any abort build): $lib" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "unwind invariant check FAILED" >&2
  exit 1
fi
echo "unwind invariant OK: panic=\"unwind\" pinned in [profile.release] + harness-core backstop present"
