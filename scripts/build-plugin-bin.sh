#!/usr/bin/env bash
# Build a release binary for one workspace member and stage it into that
# crate's bin/ as <name>-<os>-<arch> — the name the bin/<name> launcher looks
# for. The committed per-platform binaries are what `/plugin install` ships, so
# end users need neither cargo nor an API key.
#
# Usage:
#   scripts/build-plugin-bin.sh <plugin>                 # build for the host platform
#   scripts/build-plugin-bin.sh <plugin> <rust-target>   # cross-build, e.g. x86_64-apple-darwin
#   scripts/build-plugin-bin.sh <plugin> <rust-target> <bin-name>   # if bin != crate name
#
# Run from the workspace root. To produce macOS binaries, run this ON A MAC.
set -euo pipefail
cd "$(dirname "$0")/.."

plugin="${1:?usage: build-plugin-bin.sh <crate-dir> [rust-target] [bin-name]}"
target="${2:-}"
# The cargo package name can differ from the crate directory, so read it from
# the member manifest; `-p` and the artifact name must use the package, while
# bin/ stays under the crate dir.
# bin-name defaults to the package name.
pkg="$(sed -n 's/^name[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "crates/$plugin/Cargo.toml" | head -n1)"
pkg="${pkg:-$plugin}"
binname="${3:-$pkg}"

# Ask cargo where it actually puts artifacts rather than assuming ./target —
# CARGO_TARGET_DIR or a target-dir override in .cargo/config.toml (e.g.
# redirecting off a full C: drive under WSL) changes this.
target_dir="$(cargo metadata --no-deps --format-version=1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
target_dir="${target_dir:-$PWD/target}"

if [ -n "$target" ]; then
  rustc_triple="$target"
  cargo build --release -p "$pkg" --target "$target"
  src="$target_dir/$target/release/$binname"
else
  rustc_triple="$(rustc -vV | sed -n 's/^host: //p')"
  cargo build --release -p "$pkg"
  src="$target_dir/release/$binname"
fi

# normalize <triple> -> <os>-<arch>
case "$rustc_triple" in
  *apple-darwin*) os=darwin ;;
  *linux*)        os=linux ;;
  *windows*)      os=windows ;;
  *)              os="unknown" ;;
esac
case "$rustc_triple" in
  x86_64-*)  arch=x86_64 ;;
  aarch64-*) arch=arm64 ;;
  *)         arch="unknown" ;;
esac

dest="crates/$plugin/bin"
mkdir -p "$dest"
out="$dest/$binname-$os-$arch"
cp -f "$src" "$out"
chmod +x "$out" "$dest/$binname" 2>/dev/null || chmod +x "$out"
echo "staged $out ($(du -h "$out" | cut -f1), from $rustc_triple)"
echo "remember: git add $dest && git update-index --chmod=+x $out && commit"
