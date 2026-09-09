#!/usr/bin/env bash
# Regression test: the seed loop in `rebuild-plugins.sh` must seed a host binary
# for EVERY launcher a plugin ships, not just the first one.
#
# The bug. The seed loop resolved the launcher name with
#
#     binname=""
#     for f in "$bindir"/*; do
#       b=$(basename "$f")
#       case "$b" in *-linux-x86_64|…) continue ;; esac
#       binname="$b"; break        # <-- takes ONE launcher and stops
#     done
#
# That was correct only while every plugin shipped exactly one launcher. The
# moment a crate ships two — specguard ships `specguard` (the hook) and
# `specforge` (the source-side spec loop, specs/spec-loop.toml R4/R5) — the
# `break` picks whichever sorts first in glob order (`specforge`) and the second
# launcher is never seeded.
#
# Why that is a fail-open and not a cosmetic gap: the seed loop is the ONLY path
# that populates a freshly rolled-out version dir. `rollout-plugins.sh` rsyncs
# `crates/<name>/` into a brand-new `cache/<plugin>/<newver>/` that carries only
# launchers — per-host binaries are built per host and gitignored — so the main
# refresh loop, which globs *existing* `*-$SUF` files, finds nothing there. A
# launcher that the seed loop skips therefore execs a binary that does not
# exist. Its own wrapper then reports "no bundled binary" and the plugin is
# DARK: installed, version-consistent, and running nothing. No hook fires, so no
# finding is ever emitted — the failure is invisible rather than red, which is
# precisely the shape CLAUDE.md §3 forbids ("checked" must stay distinguishable
# from "could not check"). It is also how all 39 plugins were silently dark on
# 2026-09-07.
#
# Runs entirely under --dry-run against a temp cache. Nothing real is written.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REBUILD="$REPO/scripts/rebuild-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

# The fixture plugin and the two launchers it ships. Both names must correspond
# to real workspace binaries, because the seed loop only seeds a launcher for
# which `target/release/<launcher>` was actually built.
PLUGIN=specguard
LAUNCHERS=(specguard specforge)

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rebuild-seed-launchers.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

for l in "${LAUNCHERS[@]}"; do
  if [ ! -x "$REPO/target/release/$l" ]; then
    echo "  (building $l --release for the fixture)"
    (cd "$REPO" && cargo build --release -p "$PLUGIN" --bin "$l" --quiet) \
      || fail "could not build the $l fixture binary"
  fi
  [ -x "$REPO/target/release/$l" ] || fail "target/release/$l still missing after build"
done

# The seed loop reads the plugin's CURRENT version straight from plugin.json and
# only touches `$CACHE/<plugin>/<that version>/bin`, so the fixture dir has to
# carry the same version string.
PJ="$REPO/crates/$PLUGIN/.claude-plugin/plugin.json"
VER="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PJ" | head -1)"
[ -n "$VER" ] || fail "could not read the version out of $PJ"

# A freshly rolled-out version dir: launchers only, NO per-host binaries. That
# absence is the fixture — it is what makes the seed loop the only thing that
# can populate this dir.
TEST_CACHE="$TMP/cache/yukineko"
BINDIR="$TEST_CACHE/$PLUGIN/$VER/bin"
mkdir -p "$BINDIR"
for l in "${LAUNCHERS[@]}"; do
  cp "$REPO/crates/$PLUGIN/bin/$l" "$BINDIR/$l" \
    || fail "crates/$PLUGIN/bin/$l is missing — the fixture assumes both launchers are shipped"
  chmod +x "$BINDIR/$l"
done

OUT="$(cd "$REPO" && CLAUDE_PLUGIN_CACHE="$TEST_CACHE" \
  bash "$REBUILD" --no-clean --dry-run --only="$PLUGIN" 2>&1)"

# 1) The property: every launcher gets a host binary seeded.
MISSED=()
for l in "${LAUNCHERS[@]}"; do
  grep -qE "^cache +would seed $l-" <<<"$OUT" || MISSED+=("$l")
done
if [ ${#MISSED[@]} -gt 0 ]; then
  echo "$OUT" >&2
  fail "no host binary would be seeded for launcher(s): ${MISSED[*]} — \
those launchers deploy DARK (they exec a binary that is never placed)"
fi
pass "every launcher (${LAUNCHERS[*]}) would be seeded"

# 2) Anti-vacuity control. Assertion 1 is only meaningful if this run actually
#    reached the seed loop for the fixture; a run that skipped the plugin
#    entirely would print no "would seed" lines at all and fail loudly above,
#    but a run that seeded something ELSE would not. Require that the seeded
#    names are exactly the fixture's launchers.
SEEDED="$(grep -E '^cache +would seed ' <<<"$OUT" | sed -E 's/^cache +would seed ([^ ]+)-[^-]+-[^ ]+ .*/\1/' | sort -u)"
EXPECTED="$(printf '%s\n' "${LAUNCHERS[@]}" | sort -u)"
[ "$SEEDED" = "$EXPECTED" ] || {
  echo "seeded:   $(tr '\n' ' ' <<<"$SEEDED")" >&2
  echo "expected: $(tr '\n' ' ' <<<"$EXPECTED")" >&2
  fail "the seeded launcher set does not match the fixture"
}
pass "the seeded set is exactly the fixture's launchers (assertion 1 is not vacuous)"

echo "PASS: rebuild-plugins.sh seeds a host binary for every launcher a plugin ships"
