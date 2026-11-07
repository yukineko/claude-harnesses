#!/usr/bin/env bash
#
# test-changed-crates.sh — run `cargo test` for only the crates touched in the
# working tree. Invoked by donegate's Stop gate (see donegate.toml).
#
# WHY NOT `cargo test --workspace`: donegate runs on every Stop, so the check
# sits directly in the turn-completion path. A full 39-crate workspace run costs
# minutes and would push people toward disabling the gate — a gate that gets
# turned off protects nothing. Scoping to changed crates keeps it in the tens of
# seconds while still actually EXECUTING tests, which is what distinguishes
# donegate from the syntactic evidence check in tdd.
#
# KNOWN LIMITATION (deliberate, not an oversight): a change to a shared
# dependency — harness-core above all — is tested only in that crate, not in its
# ~39 dependents. Catching that fan-out is CI's job (coverage / determinism-gates
# run the whole workspace). This check is a fast local tripwire, not a substitute.
#
# Exit 0 when nothing relevant changed, so a docs- or script-only turn is free.

set -uo pipefail

REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "test-changed-crates: not a git repo — skipping" >&2
    exit 0
}
cd "$REPO" || exit 0

# Make cargo reachable: this runs from a hook, whose PATH does not necessarily
# include rustup's shim dir (the repo's toolchain is rustup-managed per CLAUDE.md).
if ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
    # Deliberately NOT a skip. A check that silently passes when it cannot run is
    # the exact failure this gate was configured to end (donegate sat inert for
    # months because an absent config read as "nothing to check"). In a Cargo
    # workspace, absent cargo is a broken environment, not a reason to wave a
    # turn through. donegate's max_attempts still prevents a permanent trap.
    echo "test-changed-crates: cargo not found (and \$HOME/.cargo/env did not provide it)" >&2
    echo "  the test check cannot run — fix the toolchain or disable this check in donegate.toml" >&2
    exit 1
fi

# Changed = tracked modifications vs HEAD, plus untracked files.
#
# A git invocation that FAILS must NOT be swallowed into an empty change set:
# empty reads as "no crate touched -> exit 0" below, silently passing the turn
# having tested nothing precisely because git couldn't tell us what changed —
# the exact cannot-determine -> allow fail-open this gate exists to end, and the
# TWIN of the cargo-absent fail-closed above (which this mirrors). So capture
# each command's exit status instead of piping `2>/dev/null` straight into a
# string.
diff_out=""
diff_rc=0
diff_out="$(git diff --name-only HEAD -- 2>/dev/null)" || diff_rc=$?

# `git diff HEAD` fails on an UNBORN branch (a repo with no commits yet). That is
# a legitimate, fully-DETERMINABLE state — "no baseline, everything present is
# new" — not a broken environment, so it must not fail closed. Handle it by
# treating every tracked/staged file as changed. Any OTHER git-diff failure is a
# genuine cannot-determine and falls through to the fail-closed check below.
if [ "$diff_rc" -ne 0 ] && ! git rev-parse --verify -q HEAD >/dev/null 2>&1; then
    diff_out="$(git ls-files 2>/dev/null)"
    diff_rc=0
fi

untracked_out=""
untracked_rc=0
untracked_out="$(git ls-files --others --exclude-standard 2>/dev/null)" || untracked_rc=$?

if [ "$diff_rc" -ne 0 ] || [ "$untracked_rc" -ne 0 ]; then
    # Cannot determine the changed set — fail CLOSED rather than report
    # "nothing to test". donegate's max_attempts still prevents a permanent trap.
    echo "test-changed-crates: git could not determine the changed file set (diff rc=$diff_rc, untracked rc=$untracked_rc)" >&2
    echo "  refusing to pass a turn as 'nothing to test' when the change set is unknown — fix the repo state, or disable this check in donegate.toml" >&2
    exit 1
fi

# COMMITTED-BUT-UNMERGED work (backlog 355d3b12): `git diff HEAD` sees only the
# uncommitted tree, so a worker that COMMITTED its crate changes on a task branch
# (clean tree) was reported "no crate touched". Add the files changed between the
# merge-base with the default branch (`main`) and HEAD. A merge-base that cannot
# be determined (no `main`, unrelated history, git failure) is UNDETERMINED — it
# must never read as "nothing changed" — so it fails CLOSED (exit 1) with the
# reason. An unborn HEAD has no commits at all, hence nothing committed to add.
committed_out=""
committed_rc=0
if git rev-parse --verify -q HEAD >/dev/null 2>&1; then
    mb="$(git merge-base HEAD main 2>/dev/null)" || committed_rc=$?
    if [ "$committed_rc" -eq 0 ] && [ -n "$mb" ]; then
        committed_out="$(git diff --name-only "$mb" HEAD -- 2>/dev/null)" || committed_rc=$?
    elif [ "$committed_rc" -eq 0 ]; then
        committed_rc=1
    fi
fi
if [ "$committed_rc" -ne 0 ]; then
    echo "test-changed-crates: cannot determine the merge-base of HEAD with main (rc=$committed_rc) — committed-but-unmerged changes cannot be enumerated" >&2
    echo "  refusing to pass a turn as 'nothing to test' when the change set is unknown — fix the repo state (a local 'main' branch must exist), or disable this check in donegate.toml" >&2
    exit 1
fi

changed="$(printf '%s\n%s\n%s\n' "$diff_out" "$untracked_out" "$committed_out" | sort -u)"

# crates/<dir>/... -> <dir>
dirs="$(printf '%s\n' "$changed" | sed -n 's#^crates/\([^/]*\)/.*#\1#p' | sort -u)"

if [ -z "$dirs" ]; then
    echo "test-changed-crates: no crate touched — nothing to test"
    exit 0
fi

# The cargo package name is not always the directory name, so resolve it from
# each crate's Cargo.toml rather than assuming they match.
pkgs=""
for d in $dirs; do
    manifest="crates/$d/Cargo.toml"
    [ -f "$manifest" ] || continue
    # Read `name` from the [package] section ONLY. A bare `head -1` on every
    # `name =` line picks up [[bin]]/[[bench]] entries too, which can differ from
    # the package name and would make `cargo test -p` fail or test the wrong thing.
    # A manifest that exists but cannot be read, or whose [package] name cannot be
    # extracted, is UNDETERMINED — not "skill-only". Dropping it silently used to
    # turn "could not tell which package to test" into "nothing to test", exit 0
    # (backlog f6919056). The git side above already fails closed the same way.
    if ! name="$(awk '
        /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\]/) ; next }
        in_pkg && /^[[:space:]]*name[[:space:]]*=/ {
            if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
        }
    ' "$manifest")"; then
        echo "test-changed-crates: cannot read $manifest — refusing to treat crate '$d' as untested-by-design" >&2
        exit 1
    fi
    if [ -z "$name" ]; then
        echo "test-changed-crates: no [package] name in $manifest — cannot tell which package to test" >&2
        exit 1
    fi
    pkgs="$pkgs $name"
done

if [ -z "$pkgs" ]; then
    echo "test-changed-crates: touched crate dirs have no Cargo.toml (skill-only?) — nothing to test"
    exit 0
fi

# This gate runs on every Stop, so it is the most frequent writer into the
# target-dir (which .cargo/config.toml may redirect to one fixed absolute path
# that nothing reclaims). Cap it BEFORE the tests build anything — cleaning after
# would throw away what this run just compiled and make the next Stop a full
# rebuild for nothing. Placed after the early exits above so a turn with no
# crate change pays nothing. No-op unless the cap is exceeded, and it exits 0
# even when it cannot measure. See scripts/cap-target-dir.sh.
"$REPO/scripts/cap-target-dir.sh"

echo "test-changed-crates: testing$pkgs"
# Where cap-target-dir.sh leaves its "cleaned at <epoch>" marker (best effort).
cap_marker=""
if meta="$(cargo metadata --no-deps --format-version=1)"; then
    td="$(printf '%s' "$meta" | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
    [ -n "$td" ] && cap_marker="${td%/}/.cap-cleaned-at"
fi

failed=""
env_failed=""
for p in $pkgs; do
    run_start="$(date +%s)"
    out_file="$(mktemp)"
    # stderr is merged into stdout so the output can be inspected, then replayed.
    cargo test -p "$p" --quiet >"$out_file" 2>&1
    rc=$?
    cat "$out_file"
    if [ "$rc" -ne 0 ]; then
        # Environment failure ONLY on two observed facts together: the spawn
        # NotFound signature in the output AND a cap-target-dir clean stamped
        # during this run. Either alone stays an ordinary failure.
        cleaned_at=""
        [ -n "$cap_marker" ] && [ -f "$cap_marker" ] && cleaned_at="$(cat "$cap_marker")"
        case "$cleaned_at" in '' | *[!0-9]*) cleaned_at="" ;; esac
        if grep -q 'kind: NotFound' "$out_file" && [ -n "$cleaned_at" ] && [ "$cleaned_at" -ge "$run_start" ]; then
            env_failed="$env_failed $p"
        else
            failed="$failed $p"
        fi
    fi
    rm -f "$out_file"
done

if [ -n "$env_failed" ]; then
    echo "test-changed-crates: ENVIRONMENT failure for$env_failed - undetermined, not a test verdict: the target dir was cleaned (cap-target-dir.sh cargo clean) while this run was executing, deleting the test binary (NotFound on spawn). Re-run." >&2
fi
if [ -n "$failed" ]; then
    echo "test-changed-crates: FAILED for$failed" >&2
fi
if [ -n "$failed$env_failed" ]; then
    exit 1
fi

echo "test-changed-crates: all green"
exit 0
