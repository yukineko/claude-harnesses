#!/usr/bin/env bash
# Generate a CycloneDX (JSON) Software Bill-of-Materials for a single plugin
# crate's distributed binary, using cargo-cyclonedx.
#
# cargo-cyclonedx resolves the enclosing Cargo *workspace* and — regardless of
# which member's Cargo.toml is passed via --manifest-path — writes a
# <crate>.cdx.json next to every workspace member's own Cargo.toml. That's fine
# for its own purposes but is not what we want here: this repo is a 39-crate
# workspace and we only want the SBOM for the one crate the caller asked for,
# placed under target/sbom/ (already .gitignore'd via the top-level /target
# rule) rather than scattered across crates/*/.
#
# So this script: runs `cargo cyclonedx` once (which regenerates one BOM per
# workspace member), copies only the requested crate's BOM into
# target/sbom/<crate>.cdx.json, and deletes every *.cdx.json cargo-cyclonedx
# dropped inside crates/*/ so the working tree is left clean.
#
# Usage:
#   scripts/generate-sbom.sh <crate-name>
#
# Example:
#   scripts/generate-sbom.sh condukt
#   -> target/sbom/condukt.cdx.json  (CycloneDX 1.3 JSON)
#
# Prerequisite: cargo-cyclonedx must be installed:
#   cargo install cargo-cyclonedx --locked
#
# Exit 0 on success (BOM written); exit 1 on usage error or generation failure.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

usage() {
  echo "Usage: $0 <crate-name>" >&2
  echo "  <crate-name>  a directory under crates/ that has its own Cargo.toml" >&2
  echo "" >&2
  echo "Example: $0 condukt" >&2
}

if [ "$#" -ne 1 ]; then
  echo "ERROR: expected exactly one argument (crate name), got $#" >&2
  usage
  exit 1
fi

crate="$1"
manifest="crates/$crate/Cargo.toml"

if [ -z "$crate" ]; then
  echo "ERROR: crate name must not be empty" >&2
  usage
  exit 1
fi

if [ ! -f "$manifest" ]; then
  echo "ERROR: no such crate manifest: $manifest" >&2
  echo "       (expected a directory under crates/ with its own Cargo.toml)" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found on PATH (source \$HOME/.cargo/env if using rustup)" >&2
  exit 1
fi

if ! cargo cyclonedx --version >/dev/null 2>&1; then
  echo "ERROR: cargo-cyclonedx subcommand not found." >&2
  echo "       Install it with: cargo install cargo-cyclonedx --locked" >&2
  exit 1
fi

out_dir="target/sbom"
mkdir -p "$out_dir"

# Track pre-existing *.cdx.json files under crates/ so we don't delete
# anything that was already there for an unrelated reason (there shouldn't
# be, since these are gitignored build artifacts, but be conservative).
before_list="$(mktemp)"
find crates -name '*.cdx.json' 2>/dev/null | sort >"$before_list"

echo "Generating CycloneDX SBOM(s) via cargo-cyclonedx (manifest: $manifest)..."
if ! cargo cyclonedx --manifest-path "$manifest" --format json --describe crate -qq; then
  echo "ERROR: cargo cyclonedx failed for $manifest" >&2
  rm -f "$before_list"
  exit 1
fi

generated="crates/$crate/$crate.cdx.json"
if [ ! -f "$generated" ]; then
  echo "ERROR: expected cargo-cyclonedx to produce $generated but it is missing" >&2
  rm -f "$before_list"
  exit 1
fi

dest="$out_dir/$crate.cdx.json"
cp "$generated" "$dest"

# cargo-cyclonedx regenerates a BOM for every workspace member (not just the
# requested crate). Clean up everything it newly created under crates/ so the
# working tree stays free of scattered build artifacts; leave any file that
# was already present untouched.
after_list="$(mktemp)"
find crates -name '*.cdx.json' 2>/dev/null | sort >"$after_list"
comm -13 "$before_list" "$after_list" | while IFS= read -r f; do
  rm -f "$f"
done
rm -f "$before_list" "$after_list"

if command -v python3 >/dev/null 2>&1; then
  if ! python3 -c "
import json, sys
d = json.load(open('$dest'))
assert d.get('bomFormat') == 'CycloneDX', 'missing/invalid bomFormat'
assert 'specVersion' in d, 'missing specVersion'
"; then
    echo "ERROR: generated file $dest does not look like valid CycloneDX JSON" >&2
    exit 1
  fi
fi

echo "OK: SBOM written to $dest"
