#!/usr/bin/env bash
# Tests for scripts/cap-target-dir.sh — the deterministic gate that runs
# `cargo clean` ONLY when the shared cargo target-dir has grown past a cap.
# Run: scripts/test_cap_target_dir.sh
#
# WRITTEN BY A NON-IMPLEMENTING AGENT (CLAUDE.md 2.(a)). The script under test
# did not exist when these cases were authored; the RED run is the evidence
# required by CLAUDE.md 2.(b).
#
# ---------------------------------------------------------------------------
# What is mocked, and what is deliberately NOT mocked
# ---------------------------------------------------------------------------
# MOCKED (external command boundary only): `cargo` and `rustc`, via a PATH shim.
#   * `cargo clean` must never really run here — a real clean would delete the
#     developer's actual target-dir. The shim RECORDS the invocation instead.
#   * `cargo metadata` is the only way to inject a chosen target_directory and
#     to simulate a metadata failure.
#
# NOT MOCKED (this is the logic under test): `du`, and the size comparison.
#   Stubbing `du` would delete the only thing these tests exist to observe.
#   Over-cap is instead reproduced WITHOUT a 37GB fixture, two ways:
#     * explicit-cap cases set CARGO_TARGET_CAP_MB relative to the REAL measured
#       `du -sm` of a few-MB fixture (cap = size-1 / size / size+1), so the real
#       comparison decides the outcome at its exact boundary;
#     * the DEFAULT-cap cases use `fallocate` to allocate 19000M / 20001M
#       instantly (blocks are reserved, not written), so `du -sm` really does
#       straddle 20000 without writing 20GB.
#
# ---------------------------------------------------------------------------
# The two cargo-metadata paths are NOT the same path (adjudicated spec)
# ---------------------------------------------------------------------------
# Spec 1 ("fall back to $PWD/target") and spec 6 ("a metadata failure exits
# quietly") originally read as a contradiction. The ruling that settles it:
#
#   metadata rc == 0 but the output carries no target_directory
#       -> FALL BACK to $PWD/target and evaluate the threshold normally.
#   metadata rc != 0
#       -> DO NOT fall back. exit 0 quietly, clean nothing.
#
# Rationale: a metadata failure means we are outside a cargo workspace or cargo
# itself is broken. A $PWD/target found under those conditions is not provably
# cargo's, and `cargo clean` would fail for the same reason. Cannot-determine
# must not fire a destructive operation.
#
# Both halves are pinned below ("metadata-fail-*" and "no-target_directory-*"),
# and they are pinned as a PAIR on purpose: an implementation that collapses
# both metadata outcomes into a single `|| exit 0` satisfies the failure half
# for the wrong reason, and only the fallback half catches it.
#
# ---------------------------------------------------------------------------
# What this suite does NOT prove (CLAUDE.md: ask this first in review)
# ---------------------------------------------------------------------------
#   * It does not prove `cargo clean` frees space. The clean is stubbed; only
#     the DECISION to clean is observed, never its effect.
#   * It does not prove the cap default is the literal 20000 by behaviour alone.
#     The two fallocate cases bracket it to 19001 < default <= 20002; case
#     "default-literal" greps the script for the literal and is WHITEBOX — it
#     would pass on a script that merely mentions 20000 in a comment.
#   * On the paths where cargo or du fails, it does not prove stderr is EMPTY —
#     that would be the wrong requirement (see check_no_self_report). It proves
#     only that the cap script adds no verdict of its own.
#   * It does not prove the script is fast, idempotent, or concurrency-safe.
#   * The build-entrypoint cases prove build-plugin-bin.sh REACHES the cap
#     script; they do not prove every other build entrypoint does.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/.." && pwd)"
# Overridable so a verifier can point the same suite at another copy (the
# spelling scripts/test_mutation_gate_status.sh uses for GATE_UNDER_TEST).
script="${CAP_SCRIPT_UNDER_TEST:-$here/cap-target-dir.sh}"
builder="${BUILD_ENTRYPOINT_UNDER_TEST:-$here/build-plugin-bin.sh}"
fail=0

root_tmp="$(mktemp -d "${TMPDIR:-/tmp}/captgt.XXXXXX")"
shim_dir="$root_tmp/shim"
fake_home="$root_tmp/home"
mkdir -p "$shim_dir" "$fake_home/.cargo"
# chmod 000 fixtures must be reopened before rm -rf can remove them.
cleanup() { chmod -R u+rwX "$root_tmp" 2>/dev/null; rm -rf "$root_tmp"; }
trap cleanup EXIT

if [ ! -f "$script" ]; then
  echo "MISSING: $script does not exist yet."
  echo "         Every case below therefore runs against nothing and must FAIL."
  echo "         (That is the RED observation CLAUDE.md 2.(b) requires.)"
  echo ""
fi

# The repo convention is to source ~/.cargo/env before cargo. HOME is faked so
# the real env file cannot re-prepend the real cargo ahead of the shim; the fake
# one keeps the shim first so sourcing it is harmless either way.
cat > "$fake_home/.cargo/env" <<ENVFILE
export PATH="$shim_dir:\$PATH"
ENVFILE

# Fake cargo.
#   metadata -> compact --format-version=1 JSON carrying \$STUB_TARGET_DIR
#               (or exit \$STUB_METADATA_STATUS to simulate failure)
#   clean    -> RECORDED, never performed
#   build    -> creates the artifact build-plugin-bin.sh expects
cat > "$shim_dir/cargo" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${CAP_TEST_CARGO_LOG:-/dev/null}"
case "${1:-}" in
  metadata)
    st="${STUB_METADATA_STATUS:-0}"
    if [ "$st" != "0" ]; then
      echo "error: could not find \`Cargo.toml\` in \`$PWD\`" >&2
      exit "$st"
    fi
    # rc=0 but no target_directory key: the "取れなければ" half of spec 1.
    if [ "${STUB_METADATA_NO_TARGET_DIR:-0}" = "1" ]; then
      printf '%s\n' "{\"packages\":[],\"workspace_members\":[],\"workspace_default_members\":[],\"resolve\":null,\"version\":1,\"workspace_root\":\"$PWD\",\"metadata\":null}"
      exit 0
    fi
    printf '%s\n' "{\"packages\":[],\"workspace_members\":[],\"workspace_default_members\":[],\"resolve\":null,\"target_directory\":\"${STUB_TARGET_DIR:-}\",\"version\":1,\"workspace_root\":\"$PWD\",\"metadata\":null}"
    exit 0
    ;;
  clean)
    printf '%s\n' "$*" >> "${CAP_TEST_CLEAN_LOG:-/dev/null}"
    exit 0
    ;;
  build)
    mkdir -p "${STUB_TARGET_DIR:-$PWD/target}/release"
    : > "${STUB_TARGET_DIR:-$PWD/target}/release/${STUB_BIN_NAME:-demo}"
    chmod +x "${STUB_TARGET_DIR:-$PWD/target}/release/${STUB_BIN_NAME:-demo}"
    exit 0
    ;;
esac
exit 0
SHIM
chmod +x "$shim_dir/cargo"

cat > "$shim_dir/rustc" <<'SHIM'
#!/usr/bin/env bash
if [ "${1:-}" = "-vV" ]; then
  printf 'rustc 1.99.0 (stub)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.99.0\n'
  exit 0
fi
exit 0
SHIM
chmod +x "$shim_dir/rustc"

# --- assertions -------------------------------------------------------------
ok()   { echo "ok: $1"; }
bad()  { echo "FAIL: $1"; shift; for l in "$@"; do echo "    $l"; done; fail=1; }
undet(){ echo "UNDETERMINED: $1"; shift; for l in "$@"; do echo "    $l"; done; fail=1; }

check_eq() { # name want got
  if [ "$3" = "$2" ]; then ok "$1"; else bad "$1" "want: [$2]" "got:  [$3]"; fi
}
check_empty() { # name label text
  if [ -z "$3" ]; then ok "$1"; else bad "$1" "expected $2 to be empty" "got: [$3]"; fi
}
check_nonempty() { # name label text
  if [ -n "$3" ]; then ok "$1"; else bad "$1" "expected $2 to be non-empty, was empty"; fi
}
check_contains() { # name needle haystack
  case "$3" in
    *"$2"*) ok "$1" ;;
    *) bad "$1" "expected to contain: [$2]" "in: [$3]" ;;
  esac
}
check_not_matches() { # name regex text
  if printf '%s' "$3" | grep -Eqi "$2"; then
    bad "$1" "expected NOT to match /$2/i" "in: [$3]"
  else ok "$1"; fi
}

# The rule for paths where an EXTERNAL command legitimately writes its own
# diagnostic to stderr (a failing `cargo metadata`, a failing `du`).
#
# "Quietly" means THIS SCRIPT says nothing. It does NOT mean the stderr stream
# is empty. cargo's and du's own diagnostics must survive: a failing `cargo
# metadata` signals a broken toolchain or manifest, and suppressing that is not
# this gate's business. It is also unimplementable in this repo — the
# fail-open-guard blocks the `2>/dev/null` that an empty-stderr assertion would
# force. What the script must not do is add a verdict of its own.
#
# Asserted two ways so it cannot decay into a check of nothing:
#   * no "clean" anywhere in stderr — prefix-independent, so renaming the
#     message prefix cannot silently turn this into a vacuous pass;
#   * no line carrying the script's own "cap-target-dir:" prefix, which catches
#     a self-report that manages to avoid the word "clean".
check_no_self_report() { # name stderr
  local why=""
  if printf '%s' "$2" | grep -Eqi 'clean'; then
    why="mentions a clean"
  elif printf '%s' "$2" | grep -q 'cap-target-dir:'; then
    why="carries the script's own 'cap-target-dir:' prefix"
  fi
  if [ -n "$why" ]; then
    bad "$1" "stderr $why; the script must add no verdict of its own here" \
        "stderr: [$2]"
  else ok "$1"; fi
}

# --- fixture / runner -------------------------------------------------------
# A throwaway "repo root" per case, holding a COPY of the script under test at
# scripts/, so both `$PWD` and `dirname $0/..` resolve to the same fake root no
# matter which the implementation uses to find $PWD/target.
mkfake() { # name -> path
  local fr="$root_tmp/case-$1"
  mkdir -p "$fr/scripts"
  cp "$script" "$fr/scripts/cap-target-dir.sh" 2>/dev/null
  chmod +x "$fr/scripts/cap-target-dir.sh" 2>/dev/null
  printf '%s' "$fr"
}

fill_mb() { # dir mb  (real bytes, so real du blocks)
  mkdir -p "$1"
  head -c "$(( $2 * 1024 * 1024 ))" /dev/zero > "$1/blob.bin"
}

dusize() { du -sm "$1" | cut -f1; }

# run_cap <fakeroot>; reads CARGO_TARGET_CAP_MB / STUB_* from the environment.
# Sets globals: RC, OUT, ERR, CLEANED, ERRLINES.
#
# Every artefact file is created BEFORE the run and read back WITHOUT
# 2>/dev/null, so "the run never happened" cannot masquerade as "the run was
# clean": .rc is pre-seeded with the sentinel 111, which no assertion accepts.
run_cap() {
  local fr="$1"
  : > "$fr/.clean.log"; : > "$fr/.cargo.log"; : > "$fr/.out"; : > "$fr/.err"
  echo 111 > "$fr/.rc"
  (
    cd "$fr" || exit 111
    HOME="$fake_home" PATH="$shim_dir:$PATH" \
      CAP_TEST_CLEAN_LOG="$fr/.clean.log" CAP_TEST_CARGO_LOG="$fr/.cargo.log" \
      bash "$fr/scripts/cap-target-dir.sh" > "$fr/.out" 2> "$fr/.err"
    echo $? > "$fr/.rc"
  )
  RC="$(cat "$fr/.rc")"
  OUT="$(cat "$fr/.out")"
  ERR="$(cat "$fr/.err")"
  CLEANED="$(cat "$fr/.clean.log")"
  ERRLINES="$(awk 'END{print NR}' "$fr/.err")"
}

clear_case_env() {
  unset CARGO_TARGET_CAP_MB STUB_TARGET_DIR STUB_METADATA_STATUS STUB_BIN_NAME \
        STUB_METADATA_NO_TARGET_DIR
}

echo "=== explicit cap, real du, real comparison ==============================="

# --- 1. just over the cap -> clean + exactly one stderr line ----------------
clear_case_env
fr="$(mkfake over)"; fill_mb "$fr/target" 9
size="$(dusize "$fr/target")"
export STUB_TARGET_DIR="$fr/target" CARGO_TARGET_CAP_MB=$(( size - 1 ))
run_cap "$fr"
check_eq       "over-cap: exit 0 (never fails the build)" "0" "$RC"
check_nonempty "over-cap: cargo clean WAS invoked" "clean log" "$CLEANED"
check_contains "over-cap: clean invoked as \`cargo clean\`" "clean" "$CLEANED"
check_eq       "over-cap: exactly one stderr line" "1" "$ERRLINES"
check_contains "over-cap: stderr names the measured size ($size)" "$size" "$ERR"
check_contains "over-cap: stderr names the cap ($CARGO_TARGET_CAP_MB)" "$CARGO_TARGET_CAP_MB" "$ERR"
if printf '%s' "$ERR" | grep -Eqi 'clean'; then
  ok "over-cap: stderr states that a clean happened"
else
  bad "over-cap: stderr states that a clean happened" "stderr: [$ERR]"
fi

# --- 2. exactly AT the cap -> no clean, silent ------------------------------
# "閾値以下なら" — equality is the not-clean side. This is the single case that
# separates `-gt` from `-ge`.
clear_case_env
fr="$(mkfake atcap)"; fill_mb "$fr/target" 9
size="$(dusize "$fr/target")"
export STUB_TARGET_DIR="$fr/target" CARGO_TARGET_CAP_MB="$size"
run_cap "$fr"
check_eq    "at-cap (size == cap): exit 0" "0" "$RC"
check_empty "at-cap: cargo clean NOT invoked" "clean log" "$CLEANED"
check_empty "at-cap: silent on stderr" "stderr" "$ERR"
check_empty "at-cap: silent on stdout" "stdout" "$OUT"

# --- 3. under the cap -> no clean, silent -----------------------------------
clear_case_env
fr="$(mkfake under)"; fill_mb "$fr/target" 9
size="$(dusize "$fr/target")"
export STUB_TARGET_DIR="$fr/target" CARGO_TARGET_CAP_MB=$(( size + 1 ))
run_cap "$fr"
check_eq    "under-cap: exit 0" "0" "$RC"
check_empty "under-cap: cargo clean NOT invoked" "clean log" "$CLEANED"
check_empty "under-cap: silent on stderr" "stderr" "$ERR"
check_empty "under-cap: silent on stdout" "stdout" "$OUT"

# --- 4. CARGO_TARGET_CAP_MB=0 is an explicit kill switch --------------------
# A naive `[ size -gt cap ]` treats 0 as "clean always"; this case forbids that.
clear_case_env
fr="$(mkfake capzero)"; fill_mb "$fr/target" 9
export STUB_TARGET_DIR="$fr/target" CARGO_TARGET_CAP_MB=0
run_cap "$fr"
check_eq    "cap=0: exit 0" "0" "$RC"
check_empty "cap=0: cargo clean NOT invoked (explicit disable)" "clean log" "$CLEANED"
check_empty "cap=0: no-op means silent on stderr" "stderr" "$ERR"
check_empty "cap=0: no-op means silent on stdout" "stdout" "$OUT"

echo ""
echo "=== default cap = 20000 MB (fallocate, no 20GB actually written) ========"

avail_mb="$(df -Pm "$root_tmp" | awk 'NR==2{print $4}')"
if ! [ "${avail_mb:-0}" -ge 21000 ] 2>/dev/null; then
  undet "default-cap cases could not run" \
        "need >=21000 MB free under ${TMPDIR:-/tmp}, have ${avail_mb:-unknown} MB" \
        "not skipped: an unrunnable check is not a passing check"
elif ! command -v fallocate >/dev/null 2>&1; then
  undet "default-cap cases could not run" "fallocate(1) is not installed"
else
  # 4a. below the default -> no clean
  clear_case_env
  fr="$(mkfake defunder)"; mkdir -p "$fr/target"
  if fallocate -l 19000M "$fr/target/blob.bin" 2>/dev/null; then
    size="$(dusize "$fr/target")"
    if [ "${size:-0}" -gt 20000 ]; then
      undet "default-under: fixture is wrong" "du reported ${size}M, expected <=20000"
    else
      export STUB_TARGET_DIR="$fr/target"
      run_cap "$fr"
      echo "    (fixture du = ${size} MB)"
      check_eq    "default-under (~19001M, no env var): exit 0" "0" "$RC"
      check_empty "default-under: cargo clean NOT invoked" "clean log" "$CLEANED"
      check_empty "default-under: silent on stderr" "stderr" "$ERR"
    fi
  else
    undet "default-under could not run" "fallocate -l 19000M failed"
  fi
  rm -f "$fr/target/blob.bin"

  # 4b. above the default -> clean
  clear_case_env
  fr="$(mkfake defover)"; mkdir -p "$fr/target"
  if fallocate -l 20001M "$fr/target/blob.bin" 2>/dev/null; then
    size="$(dusize "$fr/target")"
    if [ "${size:-0}" -le 20000 ]; then
      undet "default-over: fixture is wrong" "du reported ${size}M, expected >20000"
    else
      export STUB_TARGET_DIR="$fr/target"
      run_cap "$fr"
      echo "    (fixture du = ${size} MB)"
      check_eq       "default-over (~20002M, no env var): exit 0" "0" "$RC"
      check_nonempty "default-over: cargo clean WAS invoked" "clean log" "$CLEANED"
      check_eq       "default-over: exactly one stderr line" "1" "$ERRLINES"
      check_contains "default-over: stderr names the default cap 20000" "20000" "$ERR"
    fi
  else
    undet "default-over could not run" "fallocate -l 20001M failed"
  fi
  rm -f "$fr/target/blob.bin"
fi

# 4c. whitebox backstop for the exact literal. Proves less than 4a/4b: a
# comment containing 20000 would satisfy it.
if grep -q '20000' "$script" 2>/dev/null; then
  ok "default-literal: the script mentions 20000 (whitebox, weak)"
else
  bad "default-literal: the script mentions 20000 (whitebox, weak)" \
      "no literal 20000 found in $script"
fi

echo ""
echo "=== fail-safe: never break the build ===================================="

# --- 5. target_dir does not exist -------------------------------------------
clear_case_env
fr="$(mkfake absent)"
export STUB_TARGET_DIR="$fr/target"   # deliberately never created
export CARGO_TARGET_CAP_MB=1
run_cap "$fr"
check_eq    "absent target_dir: exit 0" "0" "$RC"
check_empty "absent target_dir: cargo clean NOT invoked" "clean log" "$CLEANED"
check_empty "absent target_dir: silent on stderr" "stderr" "$ERR"
check_empty "absent target_dir: silent on stdout" "stdout" "$OUT"

# --- 6. du FAILS (unreadable subdir) ----------------------------------------
# Load-bearing: a failing `du -sm` still prints a PARTIAL total on stdout
# (observed: rc=1, "5\t<dir>"), so an implementation that pipes du into `cut`
# and drops the exit status sees 5 > cap=1 and cleans on an unmeasured tree.
# Spec 6 says a du failure exits quietly, so the clean must NOT happen.
clear_case_env
fr="$(mkfake dufail)"
fill_mb "$fr/target" 4
mkdir -p "$fr/target/locked"; head -c 1048576 /dev/zero > "$fr/target/locked/x.bin"
chmod 000 "$fr/target/locked"
export STUB_TARGET_DIR="$fr/target" CARGO_TARGET_CAP_MB=1
run_cap "$fr"
chmod 755 "$fr/target/locked"
check_eq            "du failure: exit 0" "0" "$RC"
check_empty         "du failure: cargo clean NOT invoked on a partial measurement" "clean log" "$CLEANED"
# du's own "Permission denied" legitimately leaks; only a self-report is banned.
check_no_self_report "du failure: adds no verdict of its own to stderr" "$ERR"

# --- 7. cargo metadata fails (rc != 0), no $PWD/target ----------------------
clear_case_env
fr="$(mkfake metafail)"
export STUB_METADATA_STATUS=1 CARGO_TARGET_CAP_MB=1
run_cap "$fr"
check_eq            "metadata failure (no \$PWD/target): exit 0" "0" "$RC"
check_empty         "metadata failure (no \$PWD/target): cargo clean NOT invoked" "clean log" "$CLEANED"
# cargo's own "could not find Cargo.toml" is a diagnostic worth keeping; the cap
# script must merely add nothing to it. See check_no_self_report.
check_no_self_report "metadata failure (no \$PWD/target): adds no verdict of its own to stderr" "$ERR"
check_empty         "metadata failure (no \$PWD/target): silent on stdout" "stdout" "$OUT"

echo ""
echo "=== the two metadata paths are not the same path ========================"

# --- 8a. CONTROL for 8b -----------------------------------------------------
# 8b asserts a NEGATIVE ("did not clean"), which passes for free against a
# script that never cleans anything. This control runs the IDENTICAL fixture
# with metadata SUCCEEDING: it must clean. Only once this positive holds does
# 8b's negative carry information.
clear_case_env
fr="$(mkfake metafail-control)"
fill_mb "$fr/target" 9
export STUB_TARGET_DIR="$fr/target" CARGO_TARGET_CAP_MB=1
run_cap "$fr"
check_eq       "metadata-fail CONTROL: exit 0" "0" "$RC"
check_nonempty "metadata-fail CONTROL: this same fixture DOES clean when metadata succeeds" \
               "clean log" "$CLEANED"
# Directly pairs with 8b's check_no_self_report on the identical fixture: here
# the self-report MUST appear, there it must not. Without this positive, "no
# self-report" could be satisfied by a script that never reports anything.
if printf '%s' "$ERR" | grep -Eqi 'clean'; then
  ok "metadata-fail CONTROL: and it DOES report that clean on stderr"
else
  bad "metadata-fail CONTROL: and it DOES report that clean on stderr" \
      "stderr: [$ERR]"
fi

# --- 8b. metadata FAILS (rc != 0) while an oversized $PWD/target exists -----
# Adjudicated: no fallback, no clean. A metadata failure means "outside a cargo
# workspace, or cargo is broken" — $PWD/target is then not provably cargo's, and
# `cargo clean` would fail for the same reason. Cannot-determine must not fire a
# destructive operation.
clear_case_env
fr="$(mkfake metafail-pwd)"
fill_mb "$fr/target" 9
export STUB_METADATA_STATUS=1 CARGO_TARGET_CAP_MB=1
run_cap "$fr"
check_eq            "metadata failure + oversized \$PWD/target: exit 0" "0" "$RC"
check_empty         "metadata failure + oversized \$PWD/target: does NOT fall back, does NOT clean" \
                    "clean log" "$CLEANED"
check_no_self_report "metadata failure + oversized \$PWD/target: adds no verdict of its own to stderr" "$ERR"
check_empty         "metadata failure + oversized \$PWD/target: silent on stdout" "stdout" "$OUT"

# --- 9a. metadata rc=0 but NO target_directory -> fall back AND clean -------
# The mirror image of 8b, and the case that makes 8b load-bearing: an
# implementation that folds both metadata outcomes into one `|| exit 0` passes
# 8b for the wrong reason and fails here. This assertion is POSITIVE, so it
# cannot pass vacuously.
clear_case_env
fr="$(mkfake nofield-over)"
fill_mb "$fr/target" 9
size="$(dusize "$fr/target")"
export STUB_METADATA_NO_TARGET_DIR=1 CARGO_TARGET_CAP_MB=$(( size - 1 ))
run_cap "$fr"
check_eq       "no target_directory: exit 0" "0" "$RC"
check_nonempty "no target_directory: falls back to \$PWD/target and DOES clean" \
               "clean log" "$CLEANED"
check_eq       "no target_directory: exactly one stderr line" "1" "$ERRLINES"
check_contains "no target_directory: stderr names the measured size ($size)" "$size" "$ERR"

# --- 9b. the fallback still respects the threshold --------------------------
# Same fixture as 9a with the cap flipped to the not-clean side, so this
# negative is paired with 9a's positive: the fallback path must not degrade
# into "fell back, therefore clean".
clear_case_env
fr="$(mkfake nofield-under)"
fill_mb "$fr/target" 9
size="$(dusize "$fr/target")"
export STUB_METADATA_NO_TARGET_DIR=1 CARGO_TARGET_CAP_MB=$(( size + 1 ))
run_cap "$fr"
check_eq    "no target_directory, under cap: exit 0" "0" "$RC"
check_empty "no target_directory, under cap: fallback still honours the threshold" \
            "clean log" "$CLEANED"
check_empty "no target_directory, under cap: silent on stderr" "stderr" "$ERR"

echo ""
echo "=== wired into the build entrypoint ====================================="

# --- 10. structural: build-plugin-bin.sh mentions the cap script ------------
if grep -q 'cap-target-dir' "$builder" 2>/dev/null; then
  ok "wiring (structural): $builder references cap-target-dir"
else
  bad "wiring (structural): $builder references cap-target-dir" \
      "no reference found — the cap can never run from a build"
fi

# --- 11. behavioural: a real build-plugin-bin.sh run reaches the cap --------
# Strictly stronger than case 9: it also pins the ORDER. A clean issued AFTER
# `cargo build` would delete the artifact that was just built, so `clean` must
# appear before the first `build` in the cargo call log.
clear_case_env
fr="$(mkfake wiring)"
mkdir -p "$fr/crates/demo/bin"
cp "$builder" "$fr/scripts/build-plugin-bin.sh" 2>/dev/null
chmod +x "$fr/scripts/build-plugin-bin.sh" 2>/dev/null
printf '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2021"\n' \
  > "$fr/crates/demo/Cargo.toml"
fill_mb "$fr/target" 9
: > "$fr/.clean.log"; : > "$fr/.cargo.log"; : > "$fr/.out"; : > "$fr/.err"
echo 111 > "$fr/.rc"
(
  cd "$fr" || exit 111
  HOME="$fake_home" PATH="$shim_dir:$PATH" \
    STUB_TARGET_DIR="$fr/target" STUB_BIN_NAME=demo CARGO_TARGET_CAP_MB=1 \
    CAP_TEST_CLEAN_LOG="$fr/.clean.log" CAP_TEST_CARGO_LOG="$fr/.cargo.log" \
    bash "$fr/scripts/build-plugin-bin.sh" demo > "$fr/.out" 2> "$fr/.err"
  echo $? > "$fr/.rc"
)
brc="$(cat "$fr/.rc")"
bclean="$(cat "$fr/.clean.log")"
check_eq       "wiring (behavioural): build-plugin-bin.sh exits 0" "0" "$brc"
check_nonempty "wiring (behavioural): the build reached the cap and it cleaned" \
               "clean log" "$bclean"
# grep exits 1 on no-match; that is a real answer here, not an error, and an
# empty $cl is treated as "no clean was ordered" -> FAIL below.
cargolog="$(cat "$fr/.cargo.log")"
cl="$(printf '%s\n' "$cargolog" | grep -n '^clean' | head -n1 | cut -d: -f1)"
bl="$(printf '%s\n' "$cargolog" | grep -n '^build' | head -n1 | cut -d: -f1)"
if [ -n "$cl" ] && [ -n "$bl" ] && [ "$cl" -lt "$bl" ]; then
  ok "wiring: clean is issued BEFORE cargo build (would not eat the artifact)"
else
  bad "wiring: clean is issued BEFORE cargo build (would not eat the artifact)" \
      "clean at line [${cl:-none}], build at line [${bl:-none}]" \
      "cargo log: [$(printf '%s' "$cargolog" | tr '\n' '|')]"
fi

echo ""
if [ "$fail" -ne 0 ]; then
  echo "cap-target-dir: TESTS FAILED"
  exit 1
fi
echo "cap-target-dir: all tests passed"
