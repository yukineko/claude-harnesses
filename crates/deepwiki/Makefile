# Build the per-platform binaries the launcher (bin/deepwiki) dispatches to.
# `make bins` refreshes the per-platform binaries; they are gitignored and not
# committed (only the bin/deepwiki launcher is tracked).
#
# Linux cross-build uses cargo-zigbuild (no Docker). One-time setup on macOS:
#   brew install zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-gnu

LINUX_TARGET := x86_64-unknown-linux-gnu
LINUX_GLIBC  := 2.17
MAC_ARCH     := $(shell uname -m | sed 's/^arm64$$/arm64/;s/^x86_64$$/x86_64/')

.PHONY: bins mac linux test clean

bins: mac linux

mac:
	cargo build --release
	cp target/release/deepwiki bin/deepwiki-darwin-$(MAC_ARCH)
	@chmod +x bin/deepwiki bin/deepwiki-darwin-$(MAC_ARCH)
	@file bin/deepwiki-darwin-$(MAC_ARCH)

linux:
	cargo zigbuild --release --target $(LINUX_TARGET).$(LINUX_GLIBC)
	cp target/$(LINUX_TARGET)/release/deepwiki bin/deepwiki-linux-x86_64
	@chmod +x bin/deepwiki-linux-x86_64
	@file bin/deepwiki-linux-x86_64

test:
	cargo test

clean:
	cargo clean
