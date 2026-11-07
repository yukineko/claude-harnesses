#!/usr/bin/env bash
# RED (2026-11-07): the committed-unmerged block of test-changed-crates.sh and
# lint-changed-crates.sh guards with `if git rev-parse --verify -q HEAD`, which
# treats ANY non-zero (e.g. 128 = git failure) as an unborn HEAD and skips the
# committed set, so a branch with committed crate changes reads "no crate
# touched" -> exit 0. Only exit 1 means unborn (cf. donegate git.rs
# collect_committed_unmerged). Expected: both scripts exit non-zero.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REAL_GIT="$(command -v git)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/revparse-nonone.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
R="$TMP/r"
mkdir -p "$R/crates/foo/src" "$TMP/bin"
g() { "$REAL_GIT" -C "$R" "$@"; }
g init -q -b main
g config user.email t@t.t
g config user.name t
printf 'base\n' >"$R/README.md"
g add README.md
g commit -qm base
g checkout -qb task
printf 'fn main(){}\n' >"$R/crates/foo/src/main.rs"
g add crates/foo/src/main.rs
g commit -qm "crate change"
# git shim: rev-parse --verify -q HEAD fails with 128; everything else is real git.
printf '#!/usr/bin/env bash\nif [ "$*" = "rev-parse --verify -q HEAD" ]; then exit 128; fi\nexec "%s" "$@"\n' "$REAL_GIT" >"$TMP/bin/git"
chmod +x "$TMP/bin/git"
rc=0
# Control (no shim): the committed crate change is seen, so the script must NOT
# say "no crate touched" (shows the shimmed run's pass comes from the skip).
( cd "$R" && bash "$REPO/scripts/test-changed-crates.sh" >"$TMP/ctl" 2>&1 )
if grep -q "no crate touched" "$TMP/ctl"; then
  echo "SETUP FAIL: control run did not see the committed crate change" >&2
  exit 3
fi
for s in test-changed-crates.sh lint-changed-crates.sh; do
  ( cd "$R" && PATH="$TMP/bin:$PATH" bash "$REPO/scripts/$s" >"$TMP/out" 2>&1 )
  c=$?
  echo "$s exit=$c"
  if [ "$c" -eq 0 ]; then
    echo "FAIL: $s exited 0 although rev-parse failed with 128 (committed set skipped)" >&2
    rc=1
  fi
done
exit $rc
