# Makefile - common development tasks for shuvarie.
#
# Native targets (any host):
#   make build          Build the workspace (debug)
#   make run            Build and run the TUI
#   make test           Run the test suite
#   make clippy         Lint the workspace
#   make fmt            Check formatting
#   make clean          Remove build artifacts
#
# Windows targets (release, see nsis/README.md):
#   make build-win      Build the Windows release binary
#   make win-installer  Build the Windows installer (requires makensis)
#
# The Windows target defaults to x86_64-pc-windows-msvc on a Windows host and
# x86_64-pc-windows-gnu elsewhere. Override it with WIN_TARGET=<triple>.
# Cross-compiling from a POSIX host additionally needs the target's standard
# library: rustup target add <triple>

VERSION := $(shell sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml)
ifeq ($(VERSION),)
$(error failed to read the workspace version from Cargo.toml)
endif

ifeq ($(OS),Windows_NT)
WIN_TARGET ?= x86_64-pc-windows-msvc
MAKENSIS_D := /D
else
WIN_TARGET ?= x86_64-pc-windows-gnu
MAKENSIS_D := -D
endif

WIN_EXE := target/$(WIN_TARGET)/release/shuvarie.exe

.PHONY: all build run test clippy fmt clean build-win win-installer

all: build

build:
	cargo build

run:
	cargo run

test:
	cargo test

clippy:
	cargo clippy --all-targets

fmt:
	cargo fmt --check

clean:
	cargo clean

build-win:
ifneq ($(OS),Windows_NT)
	@if ! [ -d "$$(rustc --print target-libdir --target $(WIN_TARGET))" ]; then \
		echo "error: no Rust standard library for $(WIN_TARGET)." >&2; \
		echo "       install it with: rustup target add $(WIN_TARGET)" >&2; \
		exit 1; \
	fi
endif
	cargo build --release --target $(WIN_TARGET) --bin shuvarie

win-installer: build-win
	makensis $(MAKENSIS_D)VERSION="$(VERSION)" $(MAKENSIS_D)OUT_FILE="$(CURDIR)/nsis/shuvarie-setup-$(VERSION).exe" $(MAKENSIS_D)BINARY="$(CURDIR)/$(WIN_EXE)" nsis/installer.nsi
	@echo "Windows installer written to nsis/shuvarie-setup-$(VERSION).exe"

changelog:
	git-cliff -o ./CHANGELOG.md
