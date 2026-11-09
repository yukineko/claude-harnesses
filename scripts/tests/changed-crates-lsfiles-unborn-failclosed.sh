#!/usr/bin/env bash
# RED (2026-11-07): in the unborn-HEAD branch of test-changed-crates.sh
#   diff_out="$(git ls-files 2>/dev/null)"; diff_rc=0
# the ls-files exit status is discarded, so when ls-files fails the tracked
# crate files vanish from the change set, diff_rc is forced to 0 and the script
# can report "no crate touched" -> exit 0 (fail-open: cannot-determine read as
# clean). lint-changed-crates.sh keeps ls-files' rc (`|| diff_rc=$?`) and is
# included here as the intended-behaviour control.
# Expected: both scripts exit non-zero when ls-files fails on an unborn repo.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REAL_GIT="$(command -v git)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/lsfiles-unborn.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
R="$TMP/r"
mkdir -p "$R/crates/foo/src" "$TMP/bin"
g() { "$REAL_GIT" -C "$R" "$@"; }
g init -q -b main
g config user.email t@t.t
g config user.name t
printf 'fn main(){}\n' >"$R/crates/foo/src/main.rs"
g add crates/foo/src/main.rs   # staged, no commit: HEAD is unborn
# Shim: plain `git ls-files` (exact args) -> 128; everything else is real git
# (real `git diff --name-only HEAD --` fails on unborn, real rev-parse rc=1).
printf '#!/usr/bin/env bash\nif [ "$*" = "ls-files" ]; then exit 128; fi\nexec "%s" "$@"\n' "$REAL_GIT" >"$TMP/bin/git"
chmod +x "$TMP/bin/git"
rc=0
passed=0
failed=0
# Control (no shim): the staged crate change is seen -> not "no crate touched".
for s in test-changed-crates.sh lint-changed-crates.sh; do
  ( cd "$R" && bash "$REPO/scripts/$s" >"$TMP/ctl" 2>&1 )
  if grep -q -E "no crate" "$TMP/ctl"; then
    echo "SETUP FAIL: control run of $s did not see the staged crate file" >&2
    cat "$TMP/ctl" >&2
    exit 3
  fi
done
for s in test-changed-crates.sh lint-changed-crates.sh; do
  ( cd "$R" && PATH="$TMP/bin:$PATH" bash "$REPO/scripts/$s" >"$TMP/out" 2>&1 )
  c=$?
  echo "$s exit=$c"
  if [ "$c" -eq 0 ]; then
    echo "FAIL: $s exited 0 although 'git ls-files' failed (128) on an unborn HEAD" >&2
    sed 's/^/  | /' "$TMP/out" >&2
    rc=1
    failed=$((failed + 1))
  else
    passed=$((passed + 1))
  fi
done
if [ "$rc" -eq 0 ]; then
  echo "test result: ok. $passed passed; $failed failed"
else
  echo "test result: FAILED. $passed passed; $failed failed"
fi
exit $rc
