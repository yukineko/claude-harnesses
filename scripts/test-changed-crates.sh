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
changed="$(
    {
        git diff --name-only HEAD -- 2>/dev/null
        git ls-files --others --exclude-standard 2>/dev/null
    } | sort -u
)"

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
    name="$(awk '
        /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\]/) ; next }
        in_pkg && /^[[:space:]]*name[[:space:]]*=/ {
            if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
        }
    ' "$manifest")"
    [ -n "$name" ] && pkgs="$pkgs $name"
done

if [ -z "$pkgs" ]; then
    echo "test-changed-crates: touched crate dirs have no Cargo.toml (skill-only?) — nothing to test"
    exit 0
fi

echo "test-changed-crates: testing$pkgs"
failed=""
for p in $pkgs; do
    if ! cargo test -p "$p" --quiet; then
        failed="$failed $p"
    fi
done

if [ -n "$failed" ]; then
    echo "test-changed-crates: FAILED for$failed" >&2
    exit 1
fi

echo "test-changed-crates: all green"
exit 0
