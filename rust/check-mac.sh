#!/usr/bin/env bash
# Type-check the Mac app from Linux (the box): cargo check for aarch64-apple-darwin. gpui's Metal shader
# build step only runs on macOS, so its output file is stubbed for the check.
set -u
cd "$(dirname "$0")"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target-mac}
~/.cargo/bin/rustup target add aarch64-apple-darwin >/dev/null 2>&1
~/.cargo/bin/cargo check -q -p agent-box-app --target aarch64-apple-darwin 2>/dev/null
for d in "$CARGO_TARGET_DIR"/aarch64-apple-darwin/debug/build/gpui-pre-apple-*/out; do [ -d "$d" ] && touch "$d/stitched_shaders.metal"; done
~/.cargo/bin/cargo check -q -p agent-box-app --target aarch64-apple-darwin "$@"
