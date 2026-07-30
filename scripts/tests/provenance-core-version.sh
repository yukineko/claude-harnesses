#!/usr/bin/env bash
# Test that rebuild-plugins.sh records the LINKED harness-core version in
# .deployed-from.json (backlog 32170548).
#
# Why it exists, concretely: the field was first added with a typo in the awk
# argument ("$>EPO/..." instead of "$REPO/..."). `bash -n` accepted it, the
# script ran without error, and every one of the 41 manifests would have quietly
# recorded harness_core_version "unknown" — the writer's own fallback masking a
# broken writer. Nothing in the suite would have noticed until someone read a
# manifest by hand. So the version must be asserted as a VALUE, not as a
# present-and-nonempty field.
#
# The function is extracted from the real script text rather than reimplemented
# here; a copy would have carried the same typo and agreed with itself.
#
# Proves:
#   A. the manifest records the shared crate's actual [package].version;
#   B. an unreadable harness-core Cargo.toml resolves to the literal "unknown"
#      (never a fabricated version, and never a missing field — the rollout
#      checker treats "unknown" as a problem and absence as legacy);
#   C. the section scoping is real: a [dependencies] version above/below the
#      [package] one is not mistaken for it.
#
# MAC-RUNNABLE: bash 3.2, pure git + awk, no GNU coreutils, no network, no real
# ~/.claude touched.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/rebuild-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/provenance-core.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# --- extract the real write_provenance() text ------------------------------
FN="$TMP/write_provenance.sh"
awk '/^write_provenance\(\) \{$/{p=1} p{print} p && /^\}$/{exit}' "$SCRIPT" >"$FN"
grep -q '^write_provenance() {$' "$FN" \
  || fail "could not extract write_provenance() from $SCRIPT (did it get renamed?)"
grep -q 'harness_core_version' "$FN" \
  || fail "extracted write_provenance() does not mention harness_core_version at all"

SANDBOX="$TMP/repo"
mkdir -p "$SANDBOX/crates/harness-core" "$SANDBOX/crates/foo" "$TMP/vdir"
git -C "$SANDBOX" init -q
git -C "$SANDBOX" config user.email t@t.t
git -C "$SANDBOX" config user.name t

write_core_toml() { cat >"$SANDBOX/crates/harness-core/Cargo.toml"; }

run_writer() { # -> manifest text in $MANIFEST_TEXT
  rm -f "$TMP/vdir/.deployed-from.json"
  # `set -e` is active inside the real script; run the extracted function under
  # the same option so a failure cannot be papered over here.
  ( set -euo pipefail
    REPO="$SANDBOX"
    cratedir_for() { echo "$SANDBOX/crates/foo"; }
    # shellcheck source=/dev/null
    . "$FN"
    write_provenance "$TMP/vdir" foo
  ) || fail "write_provenance exited non-zero"
  MANIFEST_TEXT="$(cat "$TMP/vdir/.deployed-from.json" 2>/dev/null || true)"
  [ -n "$MANIFEST_TEXT" ] || fail "write_provenance wrote no manifest"
}

recorded_core() { # parse the field without assuming jq is installed
  printf '%s' "$MANIFEST_TEXT" \
    | sed -n 's/.*"harness_core_version":"\([^"]*\)".*/\1/p'
}

# =============================================================================
# A. the real [package].version lands in the manifest
# =============================================================================
write_core_toml <<'EOF'
[package]
name = "harness-core"
version = "9.8.7"
edition = "2021"
EOF
printf 'fn main() {}\n' >"$SANDBOX/crates/foo/main.rs"
git -C "$SANDBOX" add -A
git -C "$SANDBOX" commit -qm base

run_writer
got="$(recorded_core)"
[ "$got" = "9.8.7" ] || fail "A: expected harness_core_version 9.8.7, got '$got'; manifest=$MANIFEST_TEXT"
pass "A: the manifest records the shared crate's actual version"

# =============================================================================
# C. section scoping — a dependency's version must not be picked up
#    (checked before B so a broken-scope writer cannot pass by accident)
# =============================================================================
write_core_toml <<'EOF'
[workspace.package]
version = "0.0.1"

[package]
name = "harness-core"
version = "1.2.3"

[dependencies]
serde = "1"
version = "6.6.6"
EOF
run_writer
got="$(recorded_core)"
[ "$got" = "1.2.3" ] || fail "C: expected the [package] version 1.2.3, got '$got'; manifest=$MANIFEST_TEXT"
pass "C: only the [package] section's version is read"

# =============================================================================
# B. fault injection: an unreadable manifest resolves to "unknown"
# =============================================================================
rm -f "$SANDBOX/crates/harness-core/Cargo.toml"
run_writer
got="$(recorded_core)"
[ "$got" = "unknown" ] || fail "B: expected 'unknown' for an unreadable manifest, got '$got'; manifest=$MANIFEST_TEXT"
case "$MANIFEST_TEXT" in
  *'"harness_core_version"'*) : ;;
  *) fail "B: the field must be present-and-'unknown', not omitted; manifest=$MANIFEST_TEXT" ;;
esac
pass "B: an unreadable shared-crate manifest records 'unknown', not a guess"

echo "ALL PASS: provenance-core-version.sh"
