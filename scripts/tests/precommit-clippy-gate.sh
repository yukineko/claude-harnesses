#!/usr/bin/env bash
# Self-contained test for scripts/check-clippy-lints.py — the pre-commit
# scanner for the workspace's opt-in deny-lint CONFIGURATION.
#
# What is under test, and why each part exists
# --------------------------------------------
# The workspace root Cargo.toml declares `[workspace.lints.clippy]` with
# unwrap_used/expect_used/panic = "deny", and a crate INHERITS them only by
# writing `[lints] workspace = true` in its own Cargo.toml.
#
# The scanner does NOT run cargo or clippy (donegate's lint-changed check owns
# clippy execution; the old clippy run here duplicated it). It inspects only the
# manifests, comparing the WORKING TREE against HEAD:
#
#   * exit 1 when a changed crate whose HEAD Cargo.toml had
#     `[lints] workspace = true` no longer has it (Parts U, U2, J), or when the
#     root `[workspace.lints.clippy]` loses a deny for unwrap_used / expect_used /
#     panic (Parts W1-W4).
#   * exit 2 for every cannot-determine case: git failures (C, N, H, O), an
#     unparsable / unreadable manifest (F, F2), and a manifest target path that
#     escapes the repository (S).
#   * exit 0 otherwise — including the anti-vacuity controls (G, X, X2, Y, R),
#     without which a scanner that always blocks would satisfy every other case.
#
# Part T is the scope pin for this rewrite: a fake `cargo` on PATH writes a
# marker file if it is ever executed, and the marker must NOT appear. Against
# the previous clippy-running scanner this case is RED (it resolved cargo from
# PATH and ran it); against the config-only scanner it is GREEN.
#
# Diff source == scanned content: selection uses `git diff --name-only HEAD`
# UNION `git ls-files --others --exclude-standard`, never `--cached`. Part J
# leaves the opt-in removal unstaged with an empty `--cached` diff and requires
# a block anyway.
#
# MAC-RUNNABLE: pure git + python3 in a mktemp sandbox. No cargo needed, no
# network, no ~/.claude and no repository file is touched.
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
echo "using scanner: $SCANNER"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/precommit-clippy-gate.XXXXXX")"
cleanup() { chmod -R u+rw "$TMP" 2>/dev/null; rm -rf "$TMP"; }
trap cleanup EXIT

CLEAN_MAIN='fn main() { println!("ok"); }'
DIRTY_MAIN='fn main() { let v: Option<u8> = Some(1); println!("{}", v.unwrap()); }'

ROOT_MANIFEST='[workspace]
members  = ["crates/gatecrate", "crates/plaincrate"]
resolver = "2"

[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic       = "deny"'

GATE_MANIFEST_HEAD='[package]
name    = "gatecrate-pkg"
version = "0.0.0"
edition = "2021"

[[bin]]
name = "gatecrate-pkg"
path = "src/main.rs"'

GATE_MANIFEST="$GATE_MANIFEST_HEAD

[lints]
workspace = true"

T=""   # current fixture repo, set by seed()

seed() { # <name> -> fresh fixture workspace at $TMP/<name>, one commit deep
  T="$TMP/$1"
  rm -rf "$T"
  mkdir -p "$T/crates/gatecrate/src" "$T/crates/plaincrate/src" "$T/crates/skillonly"
  printf '%s\n' "$ROOT_MANIFEST" >"$T/Cargo.toml"
  printf '%s\n' "$GATE_MANIFEST" >"$T/crates/gatecrate/Cargo.toml"
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
# The subcommand is located by skipping leading global options (the scanner
# calls `git -C <repo> <sub> ...`, so keying on $1 would match "-C" and never
# fire).
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
# T: the scanner must NOT execute cargo. A fake cargo on PATH writes a marker.
# The change is a source edit containing a denied .unwrap() in an opted-in
# crate: the old clippy-running scanner selected the crate and ran cargo here.
# =============================================================================
echo
echo ">>> T: fake cargo on PATH must never be executed — expect no marker, exit 0"
seed t
printf '%s\n' "$DIRTY_MAIN" >"$T/crates/gatecrate/src/main.rs"
FAKEBIN="$TMP/fakecargo-bin"; mkdir -p "$FAKEBIN"
MARKER="$TMP/cargo-was-executed.marker"
rm -f "$MARKER"
cat >"$FAKEBIN/cargo" <<EOF
#!/usr/bin/env bash
echo "fake cargo invoked: \$*" >>"$MARKER"
exit 0
EOF
chmod +x "$FAKEBIN/cargo"
cp "$FAKEBIN/cargo" "$FAKEBIN/cargo-clippy"
scan PATH="$FAKEBIN:$PATH" CHECK_CLIPPY_LINTS_CARGO="$FAKEBIN/cargo"
[ ! -e "$MARKER" ] || fail "T: the scanner executed cargo (marker written: $(cat "$MARKER"))"
pass "no cargo / clippy subprocess was executed"
[ "$RC" = "0" ] || fail "T: a source-only edit with intact [lints] config must exit 0, got $RC"
pass "source-only edit with intact [lints] config exits 0"

# =============================================================================
# A: nothing changed at all — a genuine, determined "clean" -> exit 0
# =============================================================================
echo
echo ">>> A: clean tree, nothing changed — expect exit 0"
seed a
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "A: clean tree must exit 0, got $RC"
pass "clean tree exits 0"
RC_LEGIT_EMPTY="$RC"

# =============================================================================
# B: a NON-opted-in crate's manifest changed (still no [lints]) -> exit 0
# =============================================================================
echo
echo ">>> B: plaincrate (never opted in) manifest edited — expect exit 0"
seed b
printf '%s\n' '# touched' >>"$T/crates/plaincrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "B: a crate that never opted in cannot lose the opt-in; expected 0, got $RC"
pass "never-opted-in crate is not a violation"

# =============================================================================
# U: an opted-in crate drops `[lints] workspace = true` -> exit 1
# =============================================================================
echo
echo ">>> U: gatecrate drops [lints] workspace = true — expect exit 1"
seed u
printf '%s\n' "$GATE_MANIFEST_HEAD" >"$T/crates/gatecrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "U: removing the opt-in must exit 1, got $RC"
grep -q 'crates/gatecrate' <<<"$OUT" || fail "U: the offending crate was not named"
pass "removing [lints] workspace = true blocks and names the crate"

echo
echo ">>> U2: gatecrate sets [lints] workspace = false — expect exit 1"
seed u2
printf '%s\n' "$GATE_MANIFEST_HEAD" '' '[lints]' 'workspace = false' >"$T/crates/gatecrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "U2: workspace = false must exit 1, got $RC"
pass "workspace = false blocks"

# =============================================================================
# J: diff source == scanned content. The opt-in removal is UNSTAGED and
# `git diff --cached` is empty (asserted). Must still block.
# =============================================================================
echo
echo ">>> J: opt-in removal left UNSTAGED (git diff --cached empty) — expect exit 1"
seed j
printf '%s\n' "$GATE_MANIFEST_HEAD" >"$T/crates/gatecrate/Cargo.toml"
CACHED="$("$REAL_GIT" -C "$T" diff --cached --name-only)"
[ -z "$CACHED" ] || fail "J: fixture is wrong — git diff --cached is not empty: $CACHED"
pass "precondition: git diff --cached is genuinely empty"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "J: an UNSTAGED opt-in removal must still block (exit 1), got $RC"
pass "unstaged removal blocks (selection follows the working tree)"

# =============================================================================
# G: ANTI-VACUITY — opted-in crate manifest edited, opt-in kept -> exit 0
# =============================================================================
echo
echo ">>> G: gatecrate manifest edited, opt-in kept — expect exit 0 (anti-vacuity)"
seed g
sed 's/^version = "0.0.0"/version = "0.0.1"/' "$T/crates/gatecrate/Cargo.toml" >"$T/m.tmp"
mv "$T/m.tmp" "$T/crates/gatecrate/Cargo.toml"
"$REAL_GIT" -C "$T" diff --quiet HEAD -- crates/gatecrate/Cargo.toml \
  && fail "G: fixture is wrong — the manifest did not change"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "G: an edit keeping the opt-in must exit 0, got $RC (a gate that always blocks fails here)"
pass "manifest edit keeping the opt-in exits 0"

# =============================================================================
# Y: a crate that NEWLY opts in -> exit 0 (gaining the opt-in is not a loss)
# =============================================================================
echo
echo ">>> Y: plaincrate newly opts in — expect exit 0"
seed y
printf '%s\n' '' '[lints]' 'workspace = true' >>"$T/crates/plaincrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "Y: newly opting in must exit 0, got $RC"
pass "newly opting in exits 0"

# =============================================================================
# R: a crate with no Cargo.toml at HEAD (brand-new, untracked) -> exit 0
# =============================================================================
echo
echo ">>> R: brand-new untracked crate without [lints] — expect exit 0"
seed r
mkdir -p "$T/crates/newcrate/src"
printf '%s\n' '[package]' 'name = "newcrate"' 'version = "0.0.0"' 'edition = "2021"' \
  >"$T/crates/newcrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "R: a new crate has nothing to lose; expected 0, got $RC"
pass "brand-new crate exits 0"

# =============================================================================
# D: an opted-in crate's manifest is deleted outright -> exit 0, but SAID.
# The crate stops being a cargo package; that is not a lint downgrade on a
# package that still compiles. Pinned so the choice is visible, not implicit.
# =============================================================================
echo
echo ">>> D: gatecrate Cargo.toml deleted — expect exit 0 with a 'removed' note"
seed d
rm "$T/crates/gatecrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "D: a deleted crate manifest must exit 0, got $RC"
grep -q 'manifest removed' <<<"$OUT" || fail "D: the removal was not reported"
pass "deleted crate manifest exits 0 and is reported"

# =============================================================================
# W: the root [workspace.lints.clippy] loses a deny -> exit 1
# =============================================================================
for lint in unwrap_used expect_used panic; do
  echo
  echo ">>> W/$lint: root deny for $lint removed — expect exit 1"
  seed "w-$lint"
  grep -v "^$lint " "$T/Cargo.toml" >"$T/root.tmp"
  mv "$T/root.tmp" "$T/Cargo.toml"
  grep -q "^$lint " "$T/Cargo.toml" && fail "W/$lint: fixture is wrong — line still present"
  scan PATH="$PATH"
  [ "$RC" = "1" ] || fail "W/$lint: losing the root deny for $lint must exit 1, got $RC"
  grep -q "$lint" <<<"$OUT" || fail "W/$lint: the lost lint was not named"
  pass "losing the root deny for $lint blocks"
done

echo
echo ">>> W4: root unwrap_used downgraded to \"warn\" — expect exit 1"
seed w4
sed 's/^unwrap_used = "deny"/unwrap_used = "warn"/' "$T/Cargo.toml" >"$T/root.tmp"
mv "$T/root.tmp" "$T/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "1" ] || fail "W4: downgrading to warn must exit 1, got $RC"
pass "downgrade deny -> warn blocks"

# =============================================================================
# X: ANTI-VACUITY for the root — root manifest changed, denies intact -> exit 0
# X2: table form { level = "deny", priority = 1 } is still a deny -> exit 0
# =============================================================================
echo
echo ">>> X: root Cargo.toml changed, denies intact — expect exit 0"
seed x
printf '%s\n' '# touched: workspace lint table' >>"$T/Cargo.toml"
[ "$("$REAL_GIT" -C "$T" diff --name-only HEAD --)" = "Cargo.toml" ] \
  || fail "X: fixture is wrong — expected only Cargo.toml changed"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "X: root change with denies intact must exit 0, got $RC"
pass "root change keeping the denies exits 0"

echo
echo ">>> X2: root deny rewritten to table form — expect exit 0"
seed x2
sed 's/^panic       = "deny"/panic = { level = "deny", priority = 1 }/' "$T/Cargo.toml" >"$T/root.tmp"
mv "$T/root.tmp" "$T/Cargo.toml"
grep -q 'level = "deny"' "$T/Cargo.toml" || fail "X2: fixture is wrong"
scan PATH="$PATH"
[ "$RC" = "0" ] || fail "X2: table-form deny is still a deny; expected 0, got $RC"
pass "table-form deny exits 0"

# =============================================================================
# C / N / H: git subcommand failures -> UNDETERMINED, exit 2
# =============================================================================
echo
echo ">>> C: git diff fails — expect exit 2 (undetermined)"
seed c
printf '%s\n' "$GATE_MANIFEST_HEAD" >"$T/crates/gatecrate/Cargo.toml"
STUB_DIFF="$TMP/stub-diff"; make_git_stub "$STUB_DIFF" diff
scan PATH="$STUB_DIFF:$PATH"
[ "$RC" = "2" ] || fail "C: a failing git diff must exit 2, got $RC"
pass "failing git diff exits 2"
RC_UNDETERMINED="$RC"

echo
echo ">>> N: git ls-files fails — expect exit 2 (undetermined)"
seed n
STUB_LS="$TMP/stub-ls"; make_git_stub "$STUB_LS" ls-files
scan PATH="$STUB_LS:$PATH"
[ "$RC" = "2" ] || fail "N: a failing git ls-files must exit 2, got $RC"
pass "failing git ls-files exits 2"

echo
echo ">>> H: reading the HEAD manifest fails — expect exit 2 (undetermined)"
seed h
printf '%s\n' '# touched' >>"$T/crates/gatecrate/Cargo.toml"
STUB_SHOW="$TMP/stub-show"; make_git_stub "$STUB_SHOW" show
scan PATH="$STUB_SHOW:$PATH"
[ "$RC" = "2" ] || fail "H: a failing read of the HEAD manifest must exit 2, got $RC"
pass "failing HEAD manifest read exits 2"

echo
echo ">>> H2: listing the HEAD tree fails — expect exit 2 (undetermined)"
seed h2
printf '%s\n' '# touched' >>"$T/crates/gatecrate/Cargo.toml"
STUB_LT="$TMP/stub-lstree"; make_git_stub "$STUB_LT" ls-tree
scan PATH="$STUB_LT:$PATH"
[ "$RC" = "2" ] || fail "H2: a failing git ls-tree must exit 2, got $RC"
pass "failing git ls-tree exits 2"

# =============================================================================
# O: unborn branch (no HEAD) -> exit 2 (no unverified carve-out)
# =============================================================================
echo
echo ">>> O: unborn branch (no commits) — expect exit 2 (undetermined)"
T="$TMP/o"; rm -rf "$T"; mkdir -p "$T"
"$REAL_GIT" -C "$T" init -q
"$REAL_GIT" -C "$T" config user.email t@t.t
"$REAL_GIT" -C "$T" config user.name t
scan PATH="$PATH"
[ "$RC" = "2" ] || fail "O: unborn branch must exit 2, got $RC"
pass "unborn branch exits 2"

# =============================================================================
# F: a changed crate's working-tree Cargo.toml cannot be parsed -> exit 2
# F2: ... cannot be read -> exit 2
# F3: the HEAD root manifest is fine but the working-tree root is unparsable -> 2
# =============================================================================
echo
echo ">>> F: unparseable Cargo.toml in a changed crate — expect exit 2"
seed f
printf '%s\n' '[package' 'name = "broken' >"$T/crates/gatecrate/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "2" ] || fail "F: an unparseable Cargo.toml must exit 2, got $RC"
grep -qi 'cargo.toml' <<<"$OUT" || fail "F: did not name the manifest as the reason"
pass "unparseable Cargo.toml exits 2"

if [ "$(id -u)" != "0" ]; then
  echo
  echo ">>> F2: unreadable Cargo.toml in a changed crate — expect exit 2"
  seed f2
  printf '%s\n' '# touched' >>"$T/crates/gatecrate/Cargo.toml"
  chmod 000 "$T/crates/gatecrate/Cargo.toml"
  scan PATH="$PATH"
  chmod 644 "$T/crates/gatecrate/Cargo.toml"
  [ "$RC" = "2" ] || fail "F2: an unreadable Cargo.toml must exit 2, got $RC"
  pass "unreadable Cargo.toml exits 2"
else
  echo "  skip: F2 (running as root, chmod 000 does not deny reads)"
fi

echo
echo ">>> F3: unparseable root Cargo.toml — expect exit 2"
seed f3
printf '%s\n' '[workspace' >"$T/Cargo.toml"
scan PATH="$PATH"
[ "$RC" = "2" ] || fail "F3: an unparseable root Cargo.toml must exit 2, got $RC"
pass "unparseable root Cargo.toml exits 2"

# =============================================================================
# S: an opted-in crate's manifest target path escapes the REPOSITORY -> exit 2
# =============================================================================
echo
echo ">>> S: build path escaping the repo root — expect UNDETERMINED (exit 2)"
seed s
cat >"$T/crates/gatecrate/Cargo.toml" <<'EOF'
[package]
name    = "gatecrate-pkg"
version = "0.0.0"
edition = "2021"
build   = "../../../outside/build_helper.rs"

[[bin]]
name = "gatecrate-pkg"
path = "src/main.rs"

[lints]
workspace = true
EOF
scan PATH="$PATH"
[ "$RC" = "2" ] \
  || fail "S: a manifest path outside the repo must be UNDETERMINED; expected 2, got $RC"
pass "a manifest path escaping the repo resolves to cannot-determine"

# =============================================================================
# P: the two zero-finding answers are DIFFERENT exit codes.
# =============================================================================
echo
echo ">>> P: 'nothing lost' vs 'could not determine' must differ"
echo "    legitimately-clean (Part A) rc=$RC_LEGIT_EMPTY"
echo "    cannot-determine   (Part C) rc=$RC_UNDETERMINED"
[ "$RC_LEGIT_EMPTY" = "0" ] || fail "P: legitimately-clean must be 0"
[ "$RC_UNDETERMINED" = "2" ] || fail "P: cannot-determine must be 2"
pass "the two states are distinct (0 vs 2)"

echo
echo "PASS: check-clippy-lints.py never executes cargo/clippy, blocks (1) when a"
echo "      changed crate loses [lints] workspace = true or the root loses a"
echo "      deny for unwrap_used/expect_used/panic, resolves every"
echo "      cannot-determine case to 2, and passes (0) genuinely clean changes."
