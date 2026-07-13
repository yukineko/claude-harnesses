# Build the per-platform binaries the launcher (bin/runbook) dispatches to.
# Hooks call ${CLAUDE_PLUGIN_ROOT}/bin/runbook, which execs runbook-<os>-<arch>.
# `make bins` refreshes both bundled targets; commit them.
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

# Native macOS build → bin/runbook-darwin-<arch>.
mac:
	cargo build --release
	cp target/release/runbook bin/runbook-darwin-$(MAC_ARCH)
	@chmod +x bin/runbook bin/runbook-darwin-$(MAC_ARCH)
	@file bin/runbook-darwin-$(MAC_ARCH)

# Cross-compile Linux x86_64 → bin/runbook-linux-x86_64.
linux:
	cargo zigbuild --release --target $(LINUX_TARGET).$(LINUX_GLIBC)
	cp target/$(LINUX_TARGET)/release/runbook bin/runbook-linux-x86_64
	@chmod +x bin/runbook-linux-x86_64
	@file bin/runbook-linux-x86_64

test:
	cargo test

clean:
	cargo clean
