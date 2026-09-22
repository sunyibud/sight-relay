# Sight Relay

面向 macOS 与 Windows 的自托管 AI 截图题目解析工具。Capture 负责截图和上传，Rust Server 负责账号、任务、图片存储、LLM 调用和实时推送，浏览器负责展示结果。

用户操作请先阅读：[Sight Relay 使用说明](docs/SightRelay-使用说明.md)。

- 英文说明：[README.md](README.md)
- Capture 发布页：[Gitea Releases](https://git.sunyibud.online/sunyibud/sight-relay-release/releases)

## 当前能力

- macOS Capture 使用主显示器截图；Windows Capture 可选择显示器。
- 单题快捷键：macOS `Option+A`，Windows `Alt+A`。
- 题目组快捷键：`Option/Alt+S` 加入当前截图，`Option/Alt+Z` 提交已有题目组，`Option/Alt+X` 取消并清空未提交题目组。
- 自定义鼠标按键只触发单题立即解析，不改变题目组状态。
- 题目组允许非连续页面、重复或重叠截图，使用独立 Prompt 合并解析。
- PC 与移动端主界面实时展示上传、解析、完成状态，并支持题目组图片切换和 Markdown 渲染。
- 图片历史显示解析摘要；点击后可查看完整解析和题目组图片。
- 管理员可以管理用户、Capture 设备、LLM 配置、单题/题目组 Prompt 和使用记录；普通用户只显示自己的 Capture 设备入口。
- Capture 启动时后台检查 Gitea 最新 Release；发现新版本后显示下载按钮，不会阻塞截图功能。

## 架构

```text
macOS / Windows Capture
  ├─ 屏幕截图、ROI 裁剪、设备 Token 认证
  ├─ 单题上传或题目组追加/提交/取消
  └─ WebSocket 心跳
              │
              ▼
Rust Server（Axum + SQLite）
  ├─ 登录、用户角色和设备隔离
  ├─ 本地文件或 MinIO/S3 图片存储
  ├─ OpenAI Chat Completions 兼容的视觉 LLM
  ├─ 单题/题目组任务与 Prompt 热切换
  └─ 浏览器 WebSocket 实时事件
              │
              ▼
PC / 移动端浏览器
```

Capture 和浏览器都主动连接 Server，Server 不主动连接 Capture。

## 环境要求

### Server

- Rust 2024 edition 工具链；建议使用当前稳定版 Rust。
- macOS 或 Linux。
- 一个支持图片输入的 OpenAI Chat Completions 兼容接口。
- 可选 MinIO/S3；不配置时使用本地文件存储。

### Capture

- macOS：Xcode Command Line Tools、Swift 编译器、`xcrun`、`hdiutil`，以及屏幕录制权限。
- Windows：Windows 10/11、Visual Studio Build Tools（桌面 C++）和 WebView2 Runtime。

## 快速启动 Server

```bash
cp .env.example .env
# 编辑 .env，至少修改 ADMIN_PASSWORD 并填写当前供应商的 API Key
cargo run -p sight-relay-server --release
```

默认监听 `0.0.0.0:8080`，浏览器访问 `http://127.0.0.1:8080`。

`ADMIN_USERNAME` 和 `ADMIN_PASSWORD` 只在数据目录中还没有用户时创建初始管理员；已有数据库不会因为修改环境变量而自动修改密码。项目不提供公开注册，普通用户由管理员在“用户管理”中创建。

配置模板：

- 本地/开发：[.env.example](.env.example)
- Debian/生产：[apps/server/server.env.example](apps/server/server.env.example)

## 构建 Capture

### macOS

```bash
./deploy/build-mac.sh
open dist/SightRelay.app
```

产物为 `dist/SightRelay.app` 和 `dist/SightRelay.dmg`。构建脚本会从 `crates/mac-capture/Cargo.toml` 读取版本并写入 macOS `Info.plist`。发布新版本时请先更新 Cargo 版本，再把 DMG 上传到 Release 页面。

### Windows

在 Developer PowerShell for VS 中执行：

```powershell
.\deploy\build-windows.ps1
Start-Process .\dist\SightRelay-Windows\SightRelay.exe
```

## 运行时快捷键

| 用途 | macOS | Windows |
| --- | --- | --- |
| 单题立即解析 | `Option+A` | `Alt+A` |
| 截图加入题目组 | `Option+S` | `Alt+S` |
| 提交题目组并开始解析 | `Option+Z` | `Alt+Z` |
| 取消并清空未提交题目组 | `Option+X` | `Alt+X` |

题目组提交不会再次截图；如果当前页面也属于题目内容，应先按 `Option/Alt+S`，再按 `Option/Alt+Z`。鼠标自定义触发只执行单题解析。

Capture 配置路径：

- macOS：`~/Library/Application Support/SightRelay/config.env`
- Windows：`%LOCALAPPDATA%\\SightRelay\\config.env`

macOS 实际截图始终使用主显示器；Windows 使用 Capture 配置中的显示器编号。ROI 使用 `0..1` 的归一化坐标。自动采集已关闭，快捷键和菜单栏/托盘“立即截图”是主要触发方式。

## Server 配置和存储

- `SIGHT_DATA_DIR`：SQLite 数据库和本地图片目录，默认 `./data`。
- `SIGHT_PROMPT_FILE`：单题 Prompt 文件，默认 `.sight-prompt.md`。
- `SIGHT_MULTI_PAGE_PROMPT_FILE`：题目组 Prompt 文件，默认 `.sight-multi-page-prompt.md`。
- `SIGHT_ANALYSIS_CONCURRENCY`：并行解析数，限制为 `1..16`，默认 `2`。
- `AI_PROVIDER`：`openai`、`dashscope`、`deepseek`、`kimi`、`zhipu`、`minimax` 或 `other`。
- 每个供应商独立支持 Base URL、代理、模型、推理/思考参数、额外 JSON 和 API Key；管理员设置页修改后热切换。
- 未配置 MinIO/S3 时，图片保存到 `${SIGHT_DATA_DIR}/captures/`；配置完整的 `MINIO_ENDPOINT`、`MINIO_ACCESS_KEY`、`MINIO_SECRET_KEY` 后改用对象存储。
- 单张上传图片上限为 10 MiB，请求体额外预留 multipart 边界空间。

数据库、上传图片、Prompt 运行副本、Token 和 `.env` 都是运行时数据，不应提交到公开仓库。

## Web 页面和权限

- `/`：登录后的 PC/移动端实时主界面。
- `/settings`：配置界面；管理员可见用户管理、LLM 配置、使用记录和 Capture 设备，普通用户只显示 Capture 设备。
- `/healthz`：健康检查。
- `/api/v1/ws`：浏览器和 Capture 的实时 WebSocket。

截图、任务、解析结果、设备和 WebSocket 事件按用户隔离。管理员配置的单题 Prompt 与题目组 Prompt 分开保存，后续任务立即使用新配置。

## 生产部署

Debian：

```bash
sudo ./deploy/debian-install.sh
sudoedit /etc/sight-relay/server.env
sudo systemctl restart sight-relay
sudo journalctl -u sight-relay -f
```

生产环境建议使用 `deploy/nginx.sight-relay.conf.example` 配置 HTTPS 和 WebSocket 反向代理，不要直接把 8080 暴露到公网。也可以使用：
项目也提供 `deploy/Dockerfile` 和 `deploy/docker-compose.yml` 作为容器化起点；启动容器时请按实际部署显式注入 Server 环境变量，并将 `/data` 持久化。使用 MinIO 时仍需在 Server 环境中填写对象存储配置。

## 验证

```bash
cargo test --workspace
cargo build --release -p sight-relay-server
./deploy/test-roi.sh       # 仅 macOS 原生 ROI 测试
```

## 安全注意事项

- 不要提交 `.env`、数据库、上传图片、设备 Token、LLM API Key 或 MinIO 密钥。
- 首次启动前必须修改 `ADMIN_PASSWORD`；不要依赖默认值。
- 公开部署必须使用 HTTPS、强管理员密码、最小权限的 MinIO 凭据和防火墙。
- 截图可能包含密码、验证码和个人信息，请在截图前确认 ROI 范围。
