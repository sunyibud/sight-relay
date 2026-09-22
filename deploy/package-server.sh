#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist/sight-relay-server-linux.tar.gz}"
BIN="${BIN:-$ROOT/target/release/sight-relay-server}"
[[ -x "$BIN" ]] || { echo "找不到 server 二进制，请先 cargo build -p sight-relay-server --release" >&2; exit 1; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/sight-relay" "$TMP/sight-relay/data"
cp "$BIN" "$TMP/sight-relay/sight-relay-server"
cp "$ROOT/apps/server/server.env.example" "$TMP/sight-relay/server.env.example"
cp "$ROOT/deploy/sight-relay.service" "$TMP/sight-relay/sight-relay.service"
cat > "$TMP/sight-relay/README.txt" <<'EOF'
运行时只需要：sight-relay-server、server.env、data/。
将 server.env 放到 /etc/sight-relay/server.env，将二进制放到 /usr/local/bin/。
EOF
mkdir -p "$(dirname "$OUT")"
tar -C "$TMP" -czf "$OUT" sight-relay
echo "created: $OUT"
