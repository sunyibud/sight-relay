#!/usr/bin/env bash
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "请使用 sudo 运行" >&2; exit 1; }
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"; APP_DIR=/opt/sight-relay; DATA_DIR=/var/lib/sight-relay; ENV_DIR=/etc/sight-relay
export DEBIAN_FRONTEND=noninteractive
apt-get update; apt-get install -y ca-certificates curl build-essential pkg-config git
if ! command -v cargo >/dev/null 2>&1; then
  # Rust 官方站点在部分服务器出口网络上可能不可达；可通过
  # RUSTUP_DIST_SERVER/RUSTUP_UPDATE_ROOT 切换到国内镜像（例如 rsproxy.cn）。
  export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://rsproxy.cn}"
  export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-https://rsproxy.cn/rustup}"
  echo "安装 Rust：$RUSTUP_DIST_SERVER"
  curl --connect-timeout 15 --max-time 600 --retry 3 \
    --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  source /root/.cargo/env
fi
id sightrelay >/dev/null 2>&1 || useradd --system --home-dir "$DATA_DIR" --shell /usr/sbin/nologin sightrelay
install -d -o sightrelay -g sightrelay "$APP_DIR" "$DATA_DIR" "$ENV_DIR"
for required in Cargo.toml Cargo.lock deploy/sight-relay.service apps/server/server.env.example apps/server/Cargo.toml apps/server/src/main.rs apps/server/src/auth.rs apps/server/assets; do
  [[ -e "$PROJECT_DIR/$required" ]] || { echo "缺少 Server 构建文件：$required" >&2; exit 1; }
done

# Server 是 Cargo 工作区成员，必须使用仓库根目录唯一的 Cargo.lock。
# `-p sight-relay-server` 只会编译 Server，不会编译 macOS/Windows Capture。
# CARGO_TARGET_DIR 保留编译缓存，重复部署不必全量重编。
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/cache/sight-relay-build}"
export CARGO_REGISTRIES_CRATES_IO_PROTOCOL="${CARGO_REGISTRIES_CRATES_IO_PROTOCOL:-sparse}"
export CARGO_REGISTRIES_CRATES_IO_INDEX="${CARGO_REGISTRIES_CRATES_IO_INDEX:-sparse+https://rsproxy.cn/index/}"
install -d -o root -g root "$CARGO_TARGET_DIR"
cd "$PROJECT_DIR"
cargo build --release --locked -p sight-relay-server
install -o root -g root -m 0755 "$CARGO_TARGET_DIR/release/sight-relay-server" "$APP_DIR/sight-relay-server"
ln -sfn "$APP_DIR/sight-relay-server" /usr/local/bin/sight-relay-server
if [[ ! -f "$ENV_DIR/server.env" ]]; then
  install -o root -g sightrelay -m 0640 "$PROJECT_DIR/apps/server/server.env.example" "$ENV_DIR/server.env"
  sed -i "s#SIGHT_DATA_DIR=.*#SIGHT_DATA_DIR=$DATA_DIR#" "$ENV_DIR/server.env"
fi
install -o root -g root -m 0644 "$PROJECT_DIR/deploy/sight-relay.service" /etc/systemd/system/sight-relay.service
systemctl daemon-reload; systemctl enable --now sight-relay; systemctl restart sight-relay
echo "完成：编辑 $ENV_DIR/server.env 后执行 systemctl restart sight-relay"
