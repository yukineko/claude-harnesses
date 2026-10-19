# Build the per-platform binaries the launcher (bin/session-insights) dispatches
# to. `make bins` refreshes both bundled targets. They are git-ignored (only the
# bin/session-insights launcher is committed); scripts/rollout-plugins.sh builds
# from source for the installed plugin.
#
# Linux cross-build uses cargo-zigbuild (no Docker). One-time setup on macOS:
#   brew install zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-gnu

LINUX_TARGET := x86_64-unknown-linux-gnu
LINUX_GLIBC  := 2.17
MAC_ARCH     := $(shell uname -m | sed 's/^arm64$$/arm64/;s/^x86_64$$/x86_64/')
NAME         := session-insights

# Cargo emits artifacts under the workspace-root target/ (this crate is a
# workspace member), not a crate-local target/. Resolve that dir so the cp
# steps find the binaries. Honors CARGO_TARGET_DIR when set.
TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(dir $(shell cargo locate-project --workspace --message-format plain))target)

.PHONY: bins mac linux test clean

bins: mac linux

mac:
	cargo build --release
	cp $(TARGET_DIR)/release/$(NAME) bin/$(NAME)-darwin-$(MAC_ARCH)
	@chmod +x bin/$(NAME) bin/$(NAME)-darwin-$(MAC_ARCH)
	@file bin/$(NAME)-darwin-$(MAC_ARCH)

linux:
	cargo zigbuild --release --target $(LINUX_TARGET).$(LINUX_GLIBC)
	cp $(TARGET_DIR)/$(LINUX_TARGET)/release/$(NAME) bin/$(NAME)-linux-x86_64
	@chmod +x bin/$(NAME)-linux-x86_64
	@file bin/$(NAME)-linux-x86_64

test:
	cargo test

clean:
	cargo clean
