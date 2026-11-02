#!/usr/bin/env bash
# Thin wrapper: the dotdot fixture race (worktree_root_symlink_dotdot.rs,
# AlreadyExists at its symlink().unwrap()) is one binary of the general repro in
# blastguard-fixed-fixture-race.sh. Exit status is that script's.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
exec bash scripts/tests/blastguard-fixed-fixture-race.sh worktree_root_symlink_dotdot
