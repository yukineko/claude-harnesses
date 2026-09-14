#!/usr/bin/env bash
# Regression test: when the "seed host binary into a FRESH current-version dir"
# pass in `rebuild-plugins.sh` DECLINES to seed a launcher, it must not decline
# SILENTLY. Declining is the dark outcome; saying nothing about it is the
# fail-open.
#
# The pass (scripts/rebuild-plugins.sh, "--- seed host binary into a FRESH
# current-version dir ---") is the ONLY path that can populate a freshly
# rolled-out cache version dir. rollout-plugins.sh rsyncs crates/<name>/ into a
# brand-new cache/<plugin>/<newver>/, and the per-host <name>-<os>-<arch>
# binaries are built per host and gitignored, so that dir arrives carrying only
# launcher shell scripts. The main refresh loop above it globs *existing*
# `*-$SUF` files, so it never visits such a dir at all. If the seed pass skips a
# launcher there, nothing else will ever place its binary.
#
# Two guards in that pass skip a launcher with zero output:
#
#   1. `src="$REL/$binname$EXT"; [ -x "$src" ] || continue`
#      No release artifact for this launcher -> `continue`. Nothing is printed,
#      `$checked` is not incremented, and the `missing` WARNING block at the end
#      is populated ONLY by the main refresh loop (which, as above, never visits
#      a fresh dir). rebuild-plugins.sh then exits 0 with a green-looking
#      summary while the fresh version dir still holds nothing but the launcher.
#
#   2. `hostbin="$bindir/$binname-$SUF$EXT"; [ -e "$hostbin" ] && continue`
#      `-e` accepts a host binary that exists WITHOUT its exec bit. The launcher
#      that consumes it requires `-x` (crates/tdd/bin/tdd: `if [ -x "$binary" ];
#      then exec ...` else "no bundled binary"), so a mode-0644 host binary is
#      exactly as unrunnable as an absent one — yet it is treated as "already
#      handled". When its bytes also match target/release/<name>, the main
#      refresh loop's `cmp -s` finds no difference and copies/chmods nothing
#      either, so the exec bit is never restored by anyone.
#
# Why this is a fail-open and not a cosmetic gap: both outcomes leave the plugin
# DARK — installed, version-consistent, running nothing. A hook that cannot
# start emits no finding, so the failure is invisible rather than red, which is
# the exact shape CLAUDE.md 3 forbids ("checked" must stay distinguishable from
# "could not check"). The seed pass was added by adf33ef8 precisely to prevent
# that state; skipping silently reintroduces it while reporting success.
#
# What is asserted: a skipped launcher must make rebuild-plugins.sh either exit
# non-zero OR name the launcher/host-binary it refused to place. Each RED case
# is paired with a control case that differs ONLY in the condition under test,
# proving the fixture really reaches the guarded branch (so a failure cannot be
# blamed on a fixture that never got there).
#
# Runs entirely under --dry-run, against a temp cache root (CLAUDE_PLUGIN_CACHE)
# and a temp CARGO_TARGET_DIR. Nothing real is written and
# ~/.claude/plugins/cache is never touched.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REBUILD="$REPO/scripts/rebuild-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

# A failed CONTROL is fatal (it means the paired assertion would be vacuous, so
# there is nothing left to measure). A failed ASSERTION is recorded and the run
# continues, so one invocation reports every guard that skips silently rather
# than only the first.
FAILURES=()
bad() { echo "FAIL: $*" >&2; FAILURES+=("$1"); }

command -v cargo >/dev/null 2>&1 || . "$HOME/.cargo/env"
command -v cargo >/dev/null 2>&1 || fail "cargo not on PATH (rebuild-plugins.sh runs \`cargo metadata\`)"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rebuild-seed-silent.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# host <os>-<arch>[.exe], matching rebuild-plugins.sh's own uname/rustc dispatch
triple="$(rustc -vV | sed -n 's/^host: //p')"
case "$triple" in
  *apple-darwin*) os=darwin ;;
  *linux*)        os=linux ;;
  *windows*)      os=windows ;;
  *)              os=unknown ;;
esac
case "$triple" in
  x86_64-*)  arch=x86_64 ;;
  aarch64-*) arch=arm64 ;;
  *)         arch=unknown ;;
esac
SUF="$os-$arch"
EXT=""
[ "$os" = windows ] && EXT=".exe"
echo "host suffix: $SUF$EXT"

# The seed pass iterates the plugins found in crates/*/.claude-plugin/plugin.json
# and reads each one's CURRENT version straight out of plugin.json, so the
# fixture must use a real plugin name and that exact version string.
PLUGIN=tdd
PJ="$REPO/crates/$PLUGIN/.claude-plugin/plugin.json"
VER="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PJ" | head -1)"
[ -n "$VER" ] || fail "could not read the version out of $PJ"
LAUNCHER_SRC="$REPO/crates/$PLUGIN/bin/$PLUGIN"
[ -f "$LAUNCHER_SRC" ] || fail "fixture source launcher missing: $LAUNCHER_SRC"
echo "fixture:     $PLUGIN/$VER"

# A stand-in for "cargo just built the workspace". --dry-run never runs
# `cargo build`; only `cargo metadata`, which honors CARGO_TARGET_DIR, so this
# fully controls what rebuild-plugins.sh sees as $REL.
FAKE_TARGET="$TMP/fake-target"
mkdir -p "$FAKE_TARGET/release"
printf 'FAKE-TDD-HOST-BIN\n' > "$FAKE_TARGET/release/$PLUGIN$EXT"
chmod +x "$FAKE_TARGET/release/$PLUGIN$EXT"

# A launcher basename that is deliberately NOT a workspace binary, so
# target/release/<it> can never exist by accident.
ORPHAN=zz-no-such-artifact

# mk_cache <label> -> prints the fresh version dir's bin path.
# Shape of a freshly rolled-out dir: bin/ with launchers only, no per-host
# binaries, no hooks/ copy yet.
mk_cache() {
  local label="$1" bindir
  bindir="$TMP/$label/yukineko/$PLUGIN/$VER/bin"
  mkdir -p "$bindir"
  echo "$bindir"
}

# run_rebuild <cache-root-of-yukineko>  -> sets OUT (stdout), ERR (stderr), RC
run_rebuild() {
  local cacheroot="$1"
  OUT="$(CARGO_TARGET_DIR="$FAKE_TARGET" CLAUDE_PLUGIN_CACHE="$cacheroot" \
    bash "$REBUILD" --no-clean --dry-run --only="$PLUGIN" 2>"$TMP/stderr.txt")"
  RC=$?
  ERR="$(cat "$TMP/stderr.txt")"
}

show() {
  echo "    --- rc=$RC"
  echo "    --- stdout:"; printf '%s\n' "$OUT" | sed 's/^/      /'
  echo "    --- stderr:"; printf '%s\n' "$ERR" | sed 's/^/      /'
}

# =============================================================================
# Case A (control for Case B): artifact PRESENT -> the pass seeds and says so.
# Proves the fixture reaches the `[ -x "$src" ]` guard at all: same cache
# layout, same launcher name, only the artifact's existence differs from B.
# =============================================================================
echo
echo "=== Case A (control): launcher WITH a release artifact is seeded loudly ==="
BINDIR_A="$(mk_cache caseA)"
cp "$LAUNCHER_SRC" "$BINDIR_A/$ORPHAN"; chmod +x "$BINDIR_A/$ORPHAN"
printf 'FAKE-ORPHAN-BIN\n' > "$FAKE_TARGET/release/$ORPHAN$EXT"
chmod +x "$FAKE_TARGET/release/$ORPHAN$EXT"
run_rebuild "$TMP/caseA/yukineko"
show
[ "$RC" -eq 0 ] || fail "control run should exit 0 (got $RC)"
grep -qF "would seed $ORPHAN-$SUF$EXT" <<<"$OUT" \
  || fail "control: expected a 'would seed $ORPHAN-$SUF$EXT' line — the fixture does NOT reach the seed pass, so Case B would be vacuous"
pass "the fixture reaches the seed pass and reports 'would seed $ORPHAN-$SUF$EXT'"
[ ! -e "$BINDIR_A/$ORPHAN-$SUF$EXT" ] || fail "--dry-run actually wrote a host binary into the fixture cache"
pass "--dry-run wrote nothing into the fixture cache"

# =============================================================================
# Case B (the defect): artifact ABSENT -> `[ -x "$src" ] || continue` skips.
# The run must not finish silently-clean: either non-zero, or the launcher it
# refused to place must be named somewhere in the output.
# =============================================================================
echo
echo "=== Case B: launcher with NO release artifact must not be skipped silently ==="
BINDIR_B="$(mk_cache caseB)"
cp "$LAUNCHER_SRC" "$BINDIR_B/$ORPHAN"; chmod +x "$BINDIR_B/$ORPHAN"
rm -f "$FAKE_TARGET/release/$ORPHAN$EXT"
[ ! -e "$FAKE_TARGET/release/$ORPHAN$EXT" ] || fail "fixture setup: $ORPHAN artifact still present"
run_rebuild "$TMP/caseB/yukineko"
show
NAMED=0
grep -qF "$ORPHAN" <<<"$OUT$ERR" && NAMED=1
if [ "$RC" -eq 0 ] && [ "$NAMED" -eq 0 ]; then
  echo "    (branch reached: seed pass visited $PLUGIN/$VER — Case A proves that — and took" >&2
  echo "     the \`[ -x \$src ] || continue\` path at scripts/rebuild-plugins.sh:379)" >&2
  bad "[B: rebuild-plugins.sh:379 \`[ -x \$src ] || continue\`] exited 0 and never mentioned \
'$ORPHAN': the fresh version dir $PLUGIN/$VER is left holding only the launcher (DARK), and the \
run looks clean. Expected either a non-zero exit or a diagnostic naming the launcher it could \
not seed."
else
  pass "a launcher with no release artifact produces rc=$RC / named=$NAMED (not silently clean)"
fi

# =============================================================================
# Case C (control for Case D): host bin ABSENT -> seeded loudly.
# Same dir, same launcher (`tdd`, a real workspace bin whose fake artifact
# exists), so C and D differ ONLY in whether a non-executable host binary is
# sitting there.
# =============================================================================
echo
echo "=== Case C (control): missing host bin for a real launcher is seeded loudly ==="
BINDIR_C="$(mk_cache caseC)"
cp "$LAUNCHER_SRC" "$BINDIR_C/$PLUGIN"; chmod +x "$BINDIR_C/$PLUGIN"
run_rebuild "$TMP/caseC/yukineko"
show
[ "$RC" -eq 0 ] || fail "control run should exit 0 (got $RC)"
grep -qF "would seed $PLUGIN-$SUF$EXT" <<<"$OUT" \
  || fail "control: expected 'would seed $PLUGIN-$SUF$EXT' — Case D would be vacuous without it"
pass "absent host bin for '$PLUGIN' is reported as 'would seed $PLUGIN-$SUF$EXT'"

# =============================================================================
# Case D (the `-e` vs `-x` defect): host bin PRESENT but not executable, with
# bytes identical to the release artifact.
#   * seed pass: `[ -e "$hostbin" ] && continue`  -> "already handled"
#   * main loop: `cmp -s "$src" "$binfile"` equal -> nothing copied, no chmod
# Nobody restores the exec bit, and the launcher's `[ -x "$binary" ]` fails, so
# the plugin is dark. The run must at least name the host binary it left
# unrunnable.
# =============================================================================
echo
echo "=== Case D: present-but-non-executable host bin must not be treated as handled ==="
BINDIR_D="$(mk_cache caseD)"
cp "$LAUNCHER_SRC" "$BINDIR_D/$PLUGIN"; chmod +x "$BINDIR_D/$PLUGIN"
cp "$FAKE_TARGET/release/$PLUGIN$EXT" "$BINDIR_D/$PLUGIN-$SUF$EXT"
chmod 644 "$BINDIR_D/$PLUGIN-$SUF$EXT"
if [ -x "$BINDIR_D/$PLUGIN-$SUF$EXT" ]; then
  # Cannot build the fixture on this filesystem (e.g. a drvfs/NTFS mount where
  # the exec bit is not representable). That is a measurement failure, not a
  # pass — report it instead of asserting anything (CLAUDE.md 3).
  fail "UNVERIFIABLE: chmod 644 did not clear the exec bit under ${TMPDIR:-/tmp} \
(filesystem cannot represent it), so the -e/-x fixture cannot be built here. \
Cases A-C above still stand."
fi
run_rebuild "$TMP/caseD/yukineko"
show
NAMED_D=0
grep -qF "$PLUGIN-$SUF$EXT" <<<"$OUT$ERR" && NAMED_D=1
if [ "$RC" -eq 0 ] && [ "$NAMED_D" -eq 0 ]; then
  echo "    (branch reached: Case C proves the seed pass visits this dir; here it took" >&2
  echo "     the \`[ -e \$hostbin ] && continue\` path at scripts/rebuild-plugins.sh:377)" >&2
  bad "[D: rebuild-plugins.sh:377 \`[ -e \$hostbin ] && continue\`] exited 0 and never mentioned \
'$PLUGIN-$SUF$EXT': a host binary without its exec bit was accepted as already handled, so the \
launcher's \`[ -x \$binary ]\` check fails and the plugin stays DARK. Expected a non-zero exit \
or a diagnostic naming it."
else
  pass "non-executable host bin produces rc=$RC / named=$NAMED_D (not silently clean)"
fi
[ -x "$BINDIR_D/$PLUGIN-$SUF$EXT" ] \
  && fail "--dry-run restored the exec bit on the fixture host binary (it must copy nothing)"
pass "--dry-run left the fixture host binary's mode alone"

echo
if [ ${#FAILURES[@]} -gt 0 ]; then
  echo "FAILED ${#FAILURES[@]} assertion(s):" >&2
  for f in "${FAILURES[@]}"; do echo "  - $f" >&2; done
  exit 1
fi
echo "PASS: rebuild-plugins.sh never skips seeding a launcher silently"
