#!/usr/bin/env bash
# RED (2026-11-07): donegate tests that run `git init -q` without `-b main`
# (src/git.rs clean_repo_is_empty_files_not_failed, src/gitenv_red.rs
# changed_files_ignores_inherited_git_dir) depend on the host's init.defaultBranch=main, because changed_files returns
# Failed when no `main` branch exists. Under init.defaultBranch=master and no
# system config they must still pass. Exit 0 = all pass, non-zero = at least one fails.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/donegate-defbranch.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
printf '[init]\n\tdefaultBranch = master\n[user]\n\tname = t\n\temail = t@t.t\n' >"$TMP/gitconfig"
export GIT_CONFIG_GLOBAL="$TMP/gitconfig" GIT_CONFIG_NOSYSTEM=1
cd "$REPO" || exit 2
rc=0
cargo test -p donegate --bin donegate -- clean_repo_is_empty_files_not_failed || rc=1
cargo test -p donegate --bin donegate -- gitenv_child_scan changed_files_ignores_inherited_git_dir || rc=1
# NOT reproduced (pass under master, so not covered here):
# tests/backlog_waiver_failopen_20261107.rs, tests/subagent_gate_e2e_20261107.rs
exit $rc
