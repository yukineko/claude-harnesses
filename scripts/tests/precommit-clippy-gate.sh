#!/usr/bin/env bash
# Self-contained test for scripts/check-clippy-lints.py — the per-crate clippy
# gate for the workspace's opt-in deny lints (unwrap_used / expect_used / panic).
#
# What is under test, and why each part exists
# --------------------------------------------
# The workspace root Cargo.toml declares `[workspace.lints.clippy]` with
# unwrap_used/expect_used/panic = "deny", and a crate INHERITS them only by
# writing `[lints] workspace = true` in its own Cargo.toml. Before this scanner
# the only local place that ran `cargo clippy` at all was donegate.toml's Stop
# hook (workspace-wide, and only in a clone where `donegate trust` has been run);
# `.githooks/pre-commit` and `.githooks/pre-push` invoke cargo nowhere. So a
# commit could introduce a denied `.unwrap()` into an opted-in gate crate and
# nothing at commit time would notice.
#
# The failure modes this pins down are the ones CLAUDE.md 3 names, so each gets
# its own observable case rather than a comment claiming it is handled:
#
#   * diff source == scanned content. clippy compiles the WORKING TREE, so the
#     crate set must come from the working tree too. A `git diff --cached`-based
#     selector lets you leave a violating edit unstaged, find no crates, and pass
#     a commit that contains the violation. Part J stages NOTHING, asserts
#     `git diff --cached` is genuinely empty, and requires a block anyway.
#   * untracked files count. A brand-new .rs file in a gate crate does not appear
#     in `git diff HEAD` at all. Part K asserts `git diff --name-only HEAD` is
#     empty for that state and requires a block anyway (selection must come from
#     the UNION of the diff and `git ls-files --others --exclude-standard`, the
#     same model scripts/test-changed-crates.sh:56-80 uses).
#   * membership is DERIVED, never hardcoded. Part B (crate without `[lints]` is
#     skipped) and Part M (the SAME crate, after opting in, is checked) are the
#     two halves of that proof. Part L pins that the resolved cargo package name
#     comes from the manifest's `[package]` section, not the directory name —
#     the fixture deliberately names dir `gatecrate` package `gatecrate-pkg`.
#   * "no opted-in crate changed" and "could not determine the crate set" are
#     DIFFERENT answers. Parts A/B exit 0, Parts C/D/E/F/N/O exit 2, and Part P
#     asserts the two codes differ. Collapsing them into one empty list is the
#     empty-set-reads-as-clean fail-open.
#   * the verdict is the subprocess EXIT STATUS. Part I runs a fake cargo that
#     prints nothing and exits 101 (must block) and a fake cargo that prints
#     compiler-looking text on stdout and exits 0 (must pass).
#   * ANTI-VACUITY. Part G modifies the gate crate in a way that is genuinely
#     clean and requires exit 0. Without it, a scanner that blocks unconditionally
#     would satisfy every other case here and the suite would prove nothing.
#
# Parts G, H, J, K, M run the REAL cargo clippy against a minimal throwaway
# workspace (measured ~0.1s per invocation on this machine, because the fixture
# crate has no dependencies) — they are not faked. Only Parts D/E/I/N/O fake or
# withhold cargo, because their subject IS the resolution/exit-status plumbing.
#
# MAC-RUNNABLE: pure git + cargo + python3 in a mktemp sandbox. No GNU coreutils
# options, no network, no ~/.claude and no repository file is touched.
#
# Exit 0 on success, non-zero on any failed assertion.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCANNER="$REPO/scripts/check-clippy-lints.py"
REAL_GIT="$(command -v git)"
PY="$(command -v python3)"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

[ -n "$REAL_GIT" ] || fail "git not found on PATH"
[ -n "$PY" ] || fail "python3 not found on PATH"
[ -f "$SCANNER" ] || fail "scripts/check-clippy-lints.py does not exist (nothing to test)"

# The real cargo, resolved the same two ways the scanner is expected to: PATH
# first, then the rustup default location (CLAUDE.md: toolchain is via rustup).
REAL_CARGO=""
if command -v cargo >/dev/null 2>&1; then
  REAL_CARGO="$(command -v cargo)"
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
  REAL_CARGO="$HOME/.cargo/bin/cargo"
fi
[ -n "$REAL_CARGO" ] || fail "no cargo found (PATH or ~/.cargo/bin) — the real-clippy parts cannot run"
echo "using scanner: $SCANNER"
echo "using cargo:   $REAL_CARGO"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/precommit-clippy-gate.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# One shared target dir so the repeated real-clippy parts hit the build cache.
export CARGO_TARGET_DIR="$TMP/cargo-target"

CLEAN_MAIN='fn main() { println!("ok"); }'
# `.unwrap()` on an Option — denied by workspace.lints.clippy.unwrap_used.
DIRTY_MAIN='fn main() { let v: Option<u8> = Some(1); println!("{}", v.unwrap()); }'

T=""   # current fixture repo, set by seed()

seed() { # <name> -> fresh fixture workspace at $TMP/<name>, one commit deep
  T="$TMP/$1"
  rm -rf "$T"
  mkdir -p "$T/crates/gatecrate/src" "$T/crates/plaincrate/src" "$T/crates/skillonly"

  # `members` is listed explicitly, not globbed: crates/skillonly has no
  # Cargo.toml (the skill-only plugin shape this repo really has), and a
  # `crates/*` glob would make cargo itself refuse to load the workspace.
  cat >"$T/Cargo.toml" <<'EOF'
[workspace]
members  = ["crates/gatecrate", "crates/plaincrate"]
resolver = "2"

[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic       = "deny"
EOF

  # Directory name (gatecrate) deliberately != package name (gatecrate-pkg).
  cat >"$T/crates/gatecrate/Cargo.toml" <<'EOF'
[package]
name    = "gatecrate-pkg"
version = "0.0.0"
edition = "2021"

[[bin]]
name = "gatecrate-pkg"
path = "src/main.rs"

[lints]
workspace = true
EOF

  # No [lints] section -> does NOT inherit the deny lints -> out of scope.
  cat >"$T/crates/plaincrate/Cargo.toml" <<'EOF'
[package]
name    = "plaincrate-pkg"
version = "0.0.0"
edition = "2021"
EOF

  printf '%s\n' "$CLEAN_MAIN" >"$T/crates/gatecrate/src/main.rs"
  printf '%s\n' "$CLEAN_MAIN" >"$T/crates/plaincrate/src/main.rs"
  printf '%s\n' "# skill only, no Cargo.toml" >"$T/crates/skillonly/SKILL.md"

  "$REAL_GIT" -C "$T" init -q
  "$REAL_GIT" -C "$T" config user.email t@t.t
  "$REAL_GIT" -C "$T" config user.name t
  "$REAL_GIT" -C "$T" config commit.gpgsign false
  "$REAL_GIT" -C "$T" add -A >/dev/null
  "$REAL_GIT" -C "$T" commit -qm init
}

OUT=""; RC=""
scan() { # run the scanner in $T with any given env assignments as arguments
  OUT="$( cd "$T" && env "$@" "$PY" "$SCANNER" 2>&1; echo "RC=$?" )"
  RC="$(grep -o 'RC=[0-9]*$' <<<"$OUT" | tail -1 | cut -d= -f2)"
  echo "$OUT" | sed 's/^/    /'
}

# A git stub that fails one subcommand and delegates the rest to real git.
#
# The subcommand is located by skipping leading global options rather than by
# reading $1. That is not cosmetic: the scanner invokes `git -C <repo> diff ...`,
# so a stub keyed on $1 matches "-C" and never fires — it would have silently
# tested nothing, and the first version of this file did exactly that until
# Part C caught it.
make_git_stub() { # <dir> <subcommand-to-fail>
  mkdir -p "$1"
  cat >"$1/git" <<STUBEOF
#!/usr/bin/env bash
want="$2"
args=("\$@")
i=0
sub=""
while [ \$i -lt \${#args[@]} ]; do
  a="\${args[\$i]}"
  case "\$a" in
    -C|-c|--git-dir|--work-tree|--namespace|--exec-path)
      i=\$((i + 2)); continue ;;
    -*)
      i=\$((i + 1)); continue ;;
    *)
      sub="\$a"; break ;;
  esac
done
if [ "\$sub" = "\$want" ]; then
  echo "fatal: simulated git \$want failure" >&2
  exit 128
fi
exec "$REAL_GIT" "\$@"
STUBEOF
  chmod +x "$1/git"
}

# =============================================================================
# A: nothing changed at all — a genuine, determined "clean" -> exit 0
# =============================================================================
echo
echo ">>> A: clean tree, nothing changed — expect exit 0"
seed a
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "A: clean tree must exit 0, got $RC"
pass "clean tree exits 0"
RC_LEGIT_EMPTY_A="$RC"

# =============================================================================
# B: only a NON-opted-in crate changed — derived membership skips it -> exit 0
# =============================================================================
echo
echo ">>> B: plaincrate (no [lints]) modified — expect exit 0, crate NOT checked"
seed b
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/plaincrate/src/main.rs"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "B: a crate without [lints] must not be checked; expected 0, got $RC"
grep -q 'plaincrate-pkg' <<<"$OUT" && fail "B: plaincrate-pkg was checked despite having no [lints] section"
pass "crate without [lints] is skipped (derivation excludes it) and exits 0"
RC_LEGIT_EMPTY="$RC"

# =============================================================================
# C: `git diff` fails -> UNDETERMINED, exit 2 (never an empty crate set)
# =============================================================================
echo
echo ">>> C: git diff fails — expect exit 2 (undetermined)"
seed c
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/gatecrate/src/main.rs"
STUB_DIFF="$TMP/stub-diff"; make_git_stub "$STUB_DIFF" diff
scan PATH="$STUB_DIFF:$PATH"
[ "$RC" = "2" ] || fail "C: a failing git diff must exit 2, got $RC"
pass "failing git diff exits 2"
RC_UNDETERMINED="$RC"

# =============================================================================
# N: `git ls-files --others` fails -> UNDETERMINED, exit 2 (the twin of C)
# =============================================================================
echo
echo ">>> N: git ls-files fails — expect exit 2 (undetermined)"
seed n
STUB_LS="$TMP/stub-ls"; make_git_stub "$STUB_LS" ls-files
scan PATH="$STUB_LS:$PATH"
[ "$RC" = "2" ] || fail "N: a failing git ls-files must exit 2, got $RC"
pass "failing git ls-files exits 2"

# =============================================================================
# O: unborn branch (no HEAD, so `git diff HEAD` fails) -> exit 2
# Pinned deliberately: this scanner does NOT carve out the unborn case the way
# scripts/test-changed-crates.sh does. Cannot-determine takes the restricted
# side, and a carve-out is only safe if something proves it is.
# =============================================================================
echo
echo ">>> O: unborn branch (no commits) — expect exit 2 (undetermined)"
T="$TMP/o"; rm -rf "$T"; mkdir -p "$T"
"$REAL_GIT" -C "$T" init -q
"$REAL_GIT" -C "$T" config user.email t@t.t
"$REAL_GIT" -C "$T" config user.name t
scan PATH="$PATH"
[ "$RC" = "2" ] || fail "O: unborn branch must exit 2, got $RC"
pass "unborn branch exits 2 (no unverified carve-out)"

# =============================================================================
# D: cargo absent from PATH (and from ~/.cargo/bin) -> exit 2
# PATH is scoped to a directory holding ONLY git, and HOME is redirected so the
# rustup fallback location does not exist either.
# =============================================================================
echo
echo ">>> D: cargo absent from PATH — expect exit 2 (undetermined)"
seed d
printf '%s\n' "$CLEAN_MAIN" >"$T/crates/gatecrate/src/main.rs.tmp"
printf '%s\n' 'fn main() { println!("still clean"); }' >"$T/crates/gatecrate/src/main.rs"
rm -f "$T/crates/gatecrate/src/main.rs.tmp"
SCOPED="$TMP/scoped-bin"; mkdir -p "$SCOPED"
ln -sf "$REAL_GIT" "$SCOPED/git"
FAKEHOME="$TMP/fakehome"; mkdir -p "$FAKEHOME"
scan PATH="$SCOPED" HOME="$FAKEHOME"
[ "$RC" = "2" ] || fail "D: cargo absent must exit 2, got $RC"
grep -qi 'cargo' <<<"$OUT" || fail "D: did not name cargo as the reason"
pass "cargo absent exits 2 and names cargo"

# =============================================================================
# E: cargo resolves but is NOT executable -> exit 2
# =============================================================================
echo
echo ">>> E: cargo path not executable — expect exit 2 (undetermined)"
seed e
printf '%s\n' 'fn main() { println!("still clean"); }' >"$T/crates/gatecrate/src/main.rs"
NOEXEC="$TMP/noexec-cargo"
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' >"$NOEXEC"
chmod 0644 "$NOEXEC"
scan PATH="$PATH" CHECK_CLIPPY_LINTS_CARGO="$NOEXEC"
[ "$RC" = "2" ] || fail "E: a non-executable cargo must exit 2, got $RC"
pass "non-executable cargo exits 2"

# =============================================================================
# F: a changed crate's Cargo.toml cannot be parsed -> exit 2
# =============================================================================
echo
echo ">>> F: unparseable Cargo.toml in a changed crate — expect exit 2"
seed f
printf '%s\n' '[package' 'name = "broken' >"$T/crates/gatecrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "2" ] || fail "F: an unparseable Cargo.toml must exit 2, got $RC"
grep -qi 'cargo.toml' <<<"$OUT" || fail "F: did not name the manifest as the reason"
pass "unparseable Cargo.toml exits 2 (not skipped as 'not opted in')"

# =============================================================================
# G: ANTI-VACUITY CONTROL — gate crate modified, change is CLEAN -> exit 0
# An implementation that always blocks FAILS here. Real cargo clippy runs.
# =============================================================================
echo
echo ">>> G: gate crate modified with CLEAN code — expect exit 0 (anti-vacuity control)"
seed g
printf '%s\n' 'fn main() { let v: Option<u8> = Some(1); println!("{}", v.unwrap_or(0)); }' \
  >"$T/crates/gatecrate/src/main.rs"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "G: a clean change to a gate crate must exit 0, got $RC (a gate that always blocks fails here)"
grep -q 'gatecrate-pkg' <<<"$OUT" || fail "G: the gate crate was not actually checked (vacuous pass?)"
pass "clean change to a checked gate crate exits 0 (and the crate WAS checked)"

# =============================================================================
# L: the checked unit is the cargo PACKAGE name from [package], not the dir name
# =============================================================================
echo
echo ">>> L: the unit handed to cargo is the [package] name, not the directory name"
grep -q -- '-p gatecrate-pkg' <<<"$OUT" || fail "L: cargo was not invoked with -p gatecrate-pkg"
grep -qE -- '-p gatecrate($|[^-])' <<<"$OUT" \
  && fail "L: cargo was invoked with the bare directory name instead of the [package] name"
pass "cargo invoked as -p gatecrate-pkg (dir 'gatecrate' -> pkg 'gatecrate-pkg')"

# =============================================================================
# H: gate crate modified with a DENIED construct -> exit 1 (real clippy)
# =============================================================================
echo
echo ">>> H: gate crate modified with .unwrap() — expect exit 1 (violation)"
seed h
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/gatecrate/src/main.rs"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "H: a denied .unwrap() in a gate crate must exit 1, got $RC"
pass "denied .unwrap() in a gate crate exits 1"

# =============================================================================
# I: the verdict is the subprocess EXIT STATUS, not stdout content
# I1: fake cargo prints NOTHING and exits 101 -> must block.
# I2: fake cargo prints compiler-looking errors on stdout and exits 0 -> passes.
# =============================================================================
echo
echo ">>> I1: fake cargo, silent, exit 101 — expect exit 1 (status, not stdout)"
seed i
printf '%s\n' "$CLEAN_MAIN" 'fn other() {}' >"$T/crates/gatecrate/src/main.rs"
SILENT="$TMP/cargo-silent"
printf '%s\n' '#!/usr/bin/env bash' 'exit 101' >"$SILENT"; chmod +x "$SILENT"
scan PATH="$PATH" CHECK_CLIPPY_LINTS_CARGO="$SILENT"
[ "$RC" = "1" ] || fail "I1: a silent non-zero checker must block (exit 1), got $RC"
pass "silent non-zero cargo blocks (verdict came from the exit status)"

echo
echo ">>> I2: fake cargo, prints 'error:' on stdout, exit 0 — expect exit 0"
LOUD="$TMP/cargo-loud"
printf '%s\n' '#!/usr/bin/env bash' 'echo "error: could not compile (this text is not the verdict)"' 'exit 0' >"$LOUD"
chmod +x "$LOUD"
scan PATH="$PATH" CHECK_CLIPPY_LINTS_CARGO="$LOUD"
[ "$RC" = "0" ] || fail "I2: verdict must follow the exit status, not stdout text; expected 0, got $RC"
pass "loud-but-zero cargo passes (stdout text is not the verdict)"

# =============================================================================
# J: diff source == scanned content. NOTHING is staged; the violating edit is
# unstaged. `git diff --cached` is empty here (asserted), so a --cached-based
# selector would find no crates and pass the commit. This must block.
# =============================================================================
echo
echo ">>> J: violating edit left UNSTAGED (git diff --cached empty) — expect exit 1"
seed j
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/gatecrate/src/main.rs"
CACHED="$("$REAL_GIT" -C "$T" diff --cached --name-only)"
[ -z "$CACHED" ] || fail "J: fixture is wrong — git diff --cached is not empty: $CACHED"
pass "precondition: git diff --cached is genuinely empty"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "J: an UNSTAGED violation must still block (exit 1), got $RC — the selector is reading the index, not the working tree"
pass "unstaged violation blocks (selection follows the working tree)"

# =============================================================================
# K: an UNTRACKED new file in a gate crate. `git diff HEAD` is empty here
# (asserted), so diff alone is insufficient — selection must union in
# `git ls-files --others --exclude-standard`. src/bin/*.rs is auto-discovered
# by cargo, so the new file is compiled without editing any tracked file.
# =============================================================================
echo
echo ">>> K: violation in an UNTRACKED new file (git diff HEAD empty) — expect exit 1"
seed k
mkdir -p "$T/crates/gatecrate/src/bin"
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/gatecrate/src/bin/extra.rs"
DIFFED="$("$REAL_GIT" -C "$T" diff --name-only HEAD --)"
[ -z "$DIFFED" ] || fail "K: fixture is wrong — git diff HEAD is not empty: $DIFFED"
pass "precondition: git diff --name-only HEAD is genuinely empty"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "K: a violation in an untracked new file must block (exit 1), got $RC — untracked files are not in the selection union"
pass "untracked new file selects the crate and blocks"

# =============================================================================
# M: a crate that NEWLY opts in is picked up automatically (no hardcoded list).
# plaincrate — skipped in Part B — gains `[lints] workspace = true` and the same
# violating body, and must now be checked and blocked.
# =============================================================================
echo
echo ">>> M: plaincrate newly opts in — expect it to be picked up, exit 1"
seed m
cat >>"$T/crates/plaincrate/Cargo.toml" <<'EOF'

[lints]
workspace = true
EOF
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/plaincrate/src/main.rs"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "M: a newly opted-in crate must be picked up automatically; expected 1, got $RC"
grep -q 'plaincrate-pkg' <<<"$OUT" || fail "M: plaincrate-pkg was not named as checked"
pass "newly opted-in crate is derived automatically and blocks"

# =============================================================================
# Q: a change to the WORKSPACE ROOT manifest puts every opted-in crate in scope,
# because that file holds the lint table they inherit. No crate file is touched
# here (asserted: the diff lists only Cargo.toml), and the code is clean, so the
# observable is the selection itself plus exit 0.
# =============================================================================
echo
echo ">>> Q: only the workspace root Cargo.toml changed — expect opted-in crates selected, exit 0"
seed q
printf '%s\n' '# touched: workspace lint table' >>"$T/Cargo.toml"
QDIFF="$("$REAL_GIT" -C "$T" diff --name-only HEAD --)"
[ "$QDIFF" = "Cargo.toml" ] || fail "Q: fixture is wrong — expected only Cargo.toml changed, got: $QDIFF"
pass "precondition: only the workspace root Cargo.toml changed"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "Q: clean crates under a root-manifest change must exit 0, got $RC"
grep -q -- '-p gatecrate-pkg' <<<"$OUT" || fail "Q: root-manifest change did not put the opted-in crate in scope"
grep -q -- '-p plaincrate-pkg' <<<"$OUT" && fail "Q: a crate without [lints] was pulled in by the root-manifest change"
pass "root-manifest change selects opted-in crates only, and exits 0 when clean"

# =============================================================================
# P: the two zero-crate answers are DIFFERENT exit codes.
# =============================================================================
echo
echo ">>> P: 'no opted-in crate changed' vs 'could not determine' must differ"
echo "    legitimately-empty (Part A) rc=$RC_LEGIT_EMPTY_A"
echo "    legitimately-empty (Part B) rc=$RC_LEGIT_EMPTY"
echo "    cannot-determine   (Part C) rc=$RC_UNDETERMINED"
[ "$RC_LEGIT_EMPTY" = "0" ] || fail "P: legitimately-empty must be 0"
[ "$RC_UNDETERMINED" = "2" ] || fail "P: cannot-determine must be 2"
[ "$RC_LEGIT_EMPTY" != "$RC_UNDETERMINED" ] || fail "P: the two states collapsed to the same exit code"
pass "the two states are distinct (0 vs 2), not one empty list"

echo
echo "PASS: check-clippy-lints.py derives its crate set from the WORKING TREE"
echo "      (diff UNION untracked, never the index), derives membership from"
echo "      [lints] workspace = true rather than a hardcoded list, takes its"
echo "      verdict from the subprocess exit status, distinguishes 'nothing"
echo "      opted-in changed' (0) from 'could not determine' (2), and still"
echo "      passes a genuinely clean change to a checked gate crate."
