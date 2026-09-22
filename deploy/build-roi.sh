#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
profile="${1:-debug}"
case "$profile" in debug|release) ;; *) echo 'Usage: build-roi.sh [debug|release]' >&2; exit 2;; esac
out="${CARGO_TARGET_DIR:-target}/$profile"
mkdir -p "$out"
xcrun swiftc -O \
  crates/mac-capture/native/roi-overlay/Geometry.swift \
  crates/mac-capture/native/roi-overlay/Overlay.swift \
  crates/mac-capture/native/roi-overlay/Main.swift \
  -o "$out/roi-overlay"
