# Never commit the bin/gauge-<os>-<arch> outputs (git-ignored, root .gitignore).
# Build the per-platform binaries the launcher (bin/gauge) dispatches to.
# Hooks call ${CLAUDE_PLUGIN_ROOT}/bin/gauge, which execs gauge-<os>-<arch>.
# Note: inside the workspace, cargo builds into the workspace-root
# target/, so the crate-local target/release/ paths below do not exist.
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

.PHONY: bins mac linux test clean

# Refresh both bundled binaries in bin/.
bins: mac linux

# Native macOS build → bin/gauge-darwin-<arch>.
mac:
	cargo build --release
	cp target/release/gauge bin/gauge-darwin-$(MAC_ARCH)
	@chmod +x bin/gauge bin/gauge-darwin-$(MAC_ARCH)
	@file bin/gauge-darwin-$(MAC_ARCH)

# Cross-compile Linux x86_64 → bin/gauge-linux-x86_64.
linux:
	cargo zigbuild --release --target $(LINUX_TARGET).$(LINUX_GLIBC)
	cp target/$(LINUX_TARGET)/release/gauge bin/gauge-linux-x86_64
	@chmod +x bin/gauge-linux-x86_64
	@file bin/gauge-linux-x86_64

test:
	cargo test

clean:
	cargo clean
