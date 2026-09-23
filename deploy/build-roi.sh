#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
profile="${1:-debug}"
case "$profile" in debug|release) ;; *) echo 'Usage: build-roi.sh [debug|release]' >&2; exit 2;; esac
out="${CARGO_TARGET_DIR:-target}/$profile"
mkdir -p "$out"
xcrun swiftc -O \
  crates/capture-client/native/roi-overlay/Geometry.swift \
  crates/capture-client/native/roi-overlay/Overlay.swift \
  crates/capture-client/native/roi-overlay/Main.swift \
  -o "$out/roi-overlay"
