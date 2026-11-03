#!/usr/bin/env bash
# Build a release binary and stage it into bin/ as fde-<os>-<arch>, the name
# the bin/fde launcher looks for. The committed per-platform binaries are
# what `/plugin install` ships, so end users need neither cargo nor an API key.
#
# Usage:
#   scripts/build-plugin-bin.sh                 # build for the host platform
#   scripts/build-plugin-bin.sh <rust-target>   # cross-build, e.g. aarch64-apple-darwin
set -euo pipefail
cd "$(dirname "$0")/.."

target="${1:-}"

target_dir="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
target_dir="${target_dir:-../../target}"

if [ -n "$target" ]; then
  rustc_triple="$target"
  cargo build --release --target "$target"
  src="$target_dir/$target/release/fde"
else
  rustc_triple="$(rustc -vV | sed -n 's/^host: //p')"
  cargo build --release
  src="$target_dir/release/fde"
fi

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

mkdir -p bin
out="bin/fde-$os-$arch"
cp -f "$src" "$out"
chmod +x "$out" bin/fde
echo "staged $out ($(du -h "$out" | cut -f1), from $rustc_triple)"
echo "remember: git add bin/ && git update-index --chmod=+x $out bin/fde && commit"
