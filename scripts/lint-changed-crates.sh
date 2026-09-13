#!/usr/bin/env bash
#
# lint-changed-crates.sh — run `cargo fmt --check` and `cargo clippy` for only
# the crates touched in the working tree. Invoked by donegate's Stop gate (see
# donegate.toml), and shaped deliberately after scripts/test-changed-crates.sh,
# which does the same scoping for `cargo test`.
#
# WHY NOT `cargo fmt --all` / `cargo clippy --workspace`: donegate runs on every
# Stop, so these checks sit directly in the turn-completion path — and the
# workspace is shared. Under the workspace-wide form, a red crate that the
# current turn never touched blocked the stop of whoever WAS working: the
# attribution bug CLAUDE.md #8 records (two sessions, one workspace; the only
# escape was the shared skip file, i.e. structural pressure toward a fail-open).
# Scoping to changed crates removes that, and keeps the clippy cost proportional
# to the change instead of to the 39-crate workspace.
#
# KNOWN LIMITATION — A REAL COVERAGE LOSS, STATED AS ONE (deliberate, not an
# oversight): a change to a shared dependency — harness-core above all — is
# linted only in that crate, not in its ~39 dependents. The twin of this comment
# in test-changed-crates.sh says "catching that fan-out is CI's job"; THAT ANSWER
# IS NOT AVAILABLE HERE. GitHub Actions is banned repo-wide (CLAUDE.md #7), and
# as of this file's date nothing else runs `cargo clippy --workspace` or
# `cargo fmt --all` automatically. What IS automatic is narrower, and only that:
#
#   .githooks/pre-push            `cargo check --workspace --all-targets` — a
#                                 workspace-wide TYPE-check at push time. It
#                                 catches a shared-crate change that breaks a
#                                 dependent's compile. It runs no lint and no
#                                 formatting check.
#   scripts/check-clippy-lints.py (pre-commit) — clippy, but ALSO scoped to the
#                                 changed crates, so it is not a backstop for
#                                 the fan-out either.
#
# So the honest statement is: after a change to a widely-depended-on crate, the
# fan-out's clippy/fmt state is UNKNOWN until someone runs the sweep by hand:
#
#   cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings
#
# That is a latency-for-coverage trade made in a gate that fires on every Stop.
# Do not let this comment drift into claiming the fan-out is covered.
#
# Exit 0 when no crate changed, so a docs- or script-only turn pays nothing.
#
# WHERE THIS DIVERGES FROM test-changed-crates.sh (both stricter, neither looser):
#   * "not a git repo" exits 1 here, not 0. Without a repo the changed set cannot
#     be determined at all, and cannot-determine resolves to the restrictive side
#     (CLAUDE.md #3); donegate invokes this from the project root, which is one.
#   * the unborn-branch fallback keeps `git ls-files`'s exit status instead of
#     forcing rc=0, so a FAILING ls-files there still falls through to the
#     fail-closed branch rather than becoming an empty "nothing to lint".
#
# Behaviour is pinned by scripts/test_lint_changed_crates.py (both directions:
# red in an untouched crate does not block, red in a touched crate does).

set -uo pipefail

# The rc is captured on its own (`|| repo_rc=$?`) rather than folded into an
# `if !`: the exit status is the only thing that distinguishes "not a repo" from
# "repo root is /", and an EMPTY answer with rc 0 is just as unusable as a
# failure, so both are judged here.
REPO=""
repo_rc=0
REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || repo_rc=$?
if [ "$repo_rc" -ne 0 ] || [ -z "$REPO" ]; then
    # See the divergence note above: no repo means no changed set, and "I could
    # not work out what changed" must not be reported as "nothing to lint".
    echo "lint-changed-crates: not a git repo — cannot determine what changed (rc=$repo_rc)" >&2
    echo "  refusing to pass a turn as 'nothing to lint' from outside a repository" >&2
    exit 1
fi
cd "$REPO" || {
    echo "lint-changed-crates: cannot cd into the repo root ($REPO)" >&2
    exit 1
}

# Make cargo reachable: this runs from a hook, whose PATH does not necessarily
# include rustup's shim dir (the repo's toolchain is rustup-managed per CLAUDE.md).
if ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
    # Deliberately NOT a skip. A check that silently passes when it cannot run
    # reports "linted, all clean" for a turn in which nothing was linted at all.
    # In a Cargo workspace, absent cargo is a broken environment, not a reason to
    # wave a turn through. donegate's max_attempts still prevents a permanent trap.
    echo "lint-changed-crates: cargo not found (and \$HOME/.cargo/env did not provide it)" >&2
    echo "  the fmt/clippy checks cannot run — fix the toolchain or disable them in donegate.toml" >&2
    exit 1
fi

# Changed = tracked modifications vs HEAD, plus untracked files. The same two
# commands, in the same order, as test-changed-crates.sh: the two gates must not
# disagree about what "changed" means.
#
# A git invocation that FAILS must NOT be swallowed into an empty change set:
# empty reads as "no crate touched -> exit 0" below, silently passing the turn
# having linted nothing precisely because git could not tell us what changed —
# the cannot-determine -> allow fail-open, and the TWIN of the cargo-absent
# fail-closed above. So capture each command's exit status instead of piping
# `2>/dev/null` straight into a string.
diff_out=""
diff_rc=0
diff_out="$(git diff --name-only HEAD -- 2>/dev/null)" || diff_rc=$?

# `git diff HEAD` fails on an UNBORN branch (a repo with no commits yet). That is
# a legitimate, fully-DETERMINABLE state — "no baseline, everything present is
# new" — not a broken environment, so it must not fail closed. Handle it by
# treating every tracked/staged file as changed. Any OTHER git-diff failure is a
# genuine cannot-determine and falls through to the fail-closed check below — as
# does a failure of the ls-files call itself, whose rc is kept rather than reset.
if [ "$diff_rc" -ne 0 ] && ! git rev-parse --verify -q HEAD >/dev/null 2>&1; then
    diff_rc=0
    diff_out="$(git ls-files)" || diff_rc=$?
fi

untracked_out=""
untracked_rc=0
untracked_out="$(git ls-files --others --exclude-standard 2>/dev/null)" || untracked_rc=$?

if [ "$diff_rc" -ne 0 ] || [ "$untracked_rc" -ne 0 ]; then
    # Cannot determine the changed set — fail CLOSED rather than report
    # "nothing to lint". donegate's max_attempts still prevents a permanent trap.
    echo "lint-changed-crates: git could not determine the changed file set (diff rc=$diff_rc, untracked rc=$untracked_rc)" >&2
    echo "  refusing to pass a turn as 'nothing to lint' when the change set is unknown — fix the repo state, or disable these checks in donegate.toml" >&2
    exit 1
fi

changed="$(printf '%s\n%s\n' "$diff_out" "$untracked_out" | sort -u)"

# crates/<dir>/... -> <dir>
dirs="$(printf '%s\n' "$changed" | sed -n 's#^crates/\([^/]*\)/.*#\1#p' | sort -u)"

if [ -z "$dirs" ]; then
    echo "lint-changed-crates: no crate touched — nothing to lint"
    exit 0
fi

# The cargo package name is not always the directory name, so resolve it from
# each crate's Cargo.toml rather than assuming they match.
pkgs=""
for d in $dirs; do
    manifest="crates/$d/Cargo.toml"
    [ -f "$manifest" ] || continue
    # Read `name` from the [package] section ONLY. A bare `head -1` on every
    # `name =` line picks up [lib]/[[bin]]/[[bench]] entries too, which can
    # differ from the package name and would make `cargo fmt -p` /
    # `cargo clippy -p` fail or lint the wrong thing.
    name="$(awk '
        /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\]/) ; next }
        in_pkg && /^[[:space:]]*name[[:space:]]*=/ {
            if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
        }
    ' "$manifest")"
    [ -n "$name" ] && pkgs="$pkgs $name"
done

if [ -z "$pkgs" ]; then
    echo "lint-changed-crates: touched crate dirs have no Cargo.toml (skill-only?) — nothing to lint"
    exit 0
fi

# clippy BUILDS, so this gate is a frequent writer into the target-dir (which
# .cargo/config.toml may redirect to one fixed absolute path that nothing
# reclaims). Cap it BEFORE clippy compiles anything — cleaning after would throw
# away what this run just built and make the next Stop a full rebuild for
# nothing. Placed after the early exits above so a turn with no crate change pays
# nothing. No-op unless the cap is exceeded, and it exits 0 even when it cannot
# measure. See scripts/cap-target-dir.sh.
"$REPO/scripts/cap-target-dir.sh"

echo "lint-changed-crates: linting$pkgs"

# Both checks run for every package before anything is reported: donegate feeds
# the tail of this output back to the agent, and a report listing every failing
# (package, check) pair is worth more than one that stops at the first. fmt is
# also cheap enough that short-circuiting it would buy nothing.
fmt_failed=""
for p in $pkgs; do
    if ! cargo fmt -p "$p" -- --check; then
        fmt_failed="$fmt_failed $p"
    fi
done

clippy_failed=""
for p in $pkgs; do
    if ! cargo clippy -p "$p" --all-targets -- -D warnings; then
        clippy_failed="$clippy_failed $p"
    fi
done

rc=0
if [ -n "$fmt_failed" ]; then
    echo "lint-changed-crates: cargo fmt --check FAILED for$fmt_failed" >&2
    rc=1
fi
if [ -n "$clippy_failed" ]; then
    echo "lint-changed-crates: cargo clippy FAILED for$clippy_failed" >&2
    rc=1
fi
if [ "$rc" -ne 0 ]; then
    exit "$rc"
fi

echo "lint-changed-crates: all green"
exit 0
