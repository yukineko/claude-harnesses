# Build the per-platform binaries the launcher (bin/beacon) dispatches to.
# Hooks call ${CLAUDE_PLUGIN_ROOT}/bin/beacon, which execs beacon-<os>-<arch>.
# `make bins` refreshes the per-platform binaries; they are gitignored and not
# committed (only the bin/beacon launcher is tracked).
#
# Linux cross-build uses cargo-zigbuild (no Docker). One-time setup on macOS:
#   brew install zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-gnu

LINUX_TARGET := x86_64-unknown-linux-gnu
# Pin an old glibc floor so the binary runs on a wide range of distros.
LINUX_GLIBC  := 2.17
# Host (macOS) arch for the darwin binary name.
MAC_ARCH     := $(shell uname -m | sed 's/^arm64$$/arm64/;s/^x86_64$$/x86_64/')
# beacon is a workspace member: cargo builds into the workspace-root target/.
TARGET_DIR   := ../../target

.PHONY: bins mac linux test clean

# Refresh both bundled binaries in bin/.
bins: mac linux

# Native macOS build → bin/beacon-darwin-<arch>.
mac:
	cargo build --release
	cp $(TARGET_DIR)/release/beacon bin/beacon-darwin-$(MAC_ARCH)
	@chmod +x bin/beacon bin/beacon-darwin-$(MAC_ARCH)
	@file bin/beacon-darwin-$(MAC_ARCH)

# Cross-compile Linux x86_64 → bin/beacon-linux-x86_64.
linux:
	cargo zigbuild --release --target $(LINUX_TARGET).$(LINUX_GLIBC)
	cp $(TARGET_DIR)/$(LINUX_TARGET)/release/beacon bin/beacon-linux-x86_64
	@chmod +x bin/beacon-linux-x86_64
	@file bin/beacon-linux-x86_64

test:
	cargo test

clean:
	cargo clean
