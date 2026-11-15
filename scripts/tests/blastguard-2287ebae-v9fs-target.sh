#!/bin/sh
# RED repro (backlog): blastguard test backlog_2287ebae fails when the cargo
# target dir is on a filesystem that does not enforce POSIX permission bits.
#
# condukt worktrees live under /mnt/c/tmp/aegis-worktrees (WSL v9fs). There,
# chmod 000 on the fixture dir is not enforced, canonicalize() returns NotFound
# instead of PermissionDenied, and the fixture precondition assert fires:
#   "fixture: the locked ancestor must make resolution fail with EACCES"
# so every blastguard change made in a condukt worktree fails donegate's
# test-changed check although the code is identical to main (observed
# 2026-11-06: passes with target on ext4 at 3cdfd72e, fails with
# CARGO_TARGET_DIR on /mnt/c at the same rev).
#
# Expected after a fix: the test passes (or reports the fixture as
# inconclusive in a way that is not a test failure) with the target on v9fs.
# Exit non-zero = reproduced. Exit 2 = the v9fs location is unavailable.
set -u
base="${BLASTGUARD_V9FS_DIR:-/mnt/c/tmp}"
[ -d "$base" ] || { echo "no v9fs dir at $base" >&2; exit 2; }
t="$base/blastguard-2287ebae-v9fs-target"
mkdir -p "$t" || exit 2
CARGO_TARGET_DIR="$t" cargo test -q -p blastguard --test backlog_2287ebae
