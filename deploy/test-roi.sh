#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sources=crates/capture-client/native/roi-overlay
xcrun swiftc "$sources/Geometry.swift" "$sources/GeometryTests.swift" -o "$work/geometry-tests"
"$work/geometry-tests"
xcrun swiftc "$sources/Geometry.swift" "$sources/Overlay.swift" "$sources/InteractionTests.swift" -o "$work/interaction-tests"
"$work/interaction-tests"
cargo test -p sight-relay-capture
