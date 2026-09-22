# Sight Relay

Self-hosted AI screenshot analysis for macOS and Windows. The native Capture client takes authenticated screenshots, the Rust Server manages users, storage, LLM jobs and live events, and the browser renders the result in Markdown.

For the complete end-user workflow, see [Sight Relay User Guide](docs/SightRelay-使用说明.md). The Chinese project overview is [README.zh-CN.md](README.zh-CN.md).

- Capture releases: [Gitea Releases](https://git.sunyibud.online/sunyibud/sight-relay-release/releases)

## Features

- macOS capture always uses the primary display; Windows can select a display.
- Single-question capture: `Option+A` on macOS, `Alt+A` on Windows.
- Multi-image question groups: `Option/Alt+S` append, `Option/Alt+Z` submit, `Option/Alt+X` cancel and clear the unsubmitted group.
- Custom mouse bindings trigger single-question capture only.
- Non-contiguous, overlapping or repeated group images are combined with a separate multi-page prompt.
- Responsive PC/mobile pages with live upload, analysis and completion states, Markdown rendering and group image navigation.
- Per-user history, device isolation, administrator-only user/LLM/usage settings and hot-swappable single/group prompts.
- Startup release check for Capture; a newer release shows a download button without blocking capture.

## Architecture

```text
Capture client (macOS / Windows)
  ├─ screen capture, ROI crop and device-token authentication
  ├─ single upload or group append/submit/cancel
  └─ heartbeat WebSocket
              │
              ▼
Rust Server (Axum + SQLite)
  ├─ login, roles and per-user isolation
  ├─ local filesystem or MinIO/S3 image storage
  ├─ OpenAI Chat Completions-compatible vision LLM
  ├─ single/group jobs and hot-swappable prompts
  └─ browser WebSocket events
              │
              ▼
PC / mobile browser
```

Capture and browsers connect to the Server; the Server does not initiate a Capture connection.

## Requirements

Server requires a current Rust stable toolchain, macOS or Linux, and an image-capable OpenAI Chat Completions-compatible API. MinIO/S3 is optional.

The macOS client requires Xcode Command Line Tools, Swift, `xcrun`, `hdiutil` and Screen Recording permission. The Windows client requires Windows 10/11, Visual Studio C++ build tools and WebView2 Runtime.

## Start the Server

```bash
cp .env.example .env
# Edit .env: change ADMIN_PASSWORD and set the API key for AI_PROVIDER.
cargo run -p sight-relay-server --release
```

The default bind address is `0.0.0.0:8080`; open `http://127.0.0.1:8080` in a browser.

`ADMIN_USERNAME` and `ADMIN_PASSWORD` are used only when the data directory has no users. Existing databases are not changed by editing those variables. There is no public registration; administrators create regular users.

Configuration templates:

- Local/development: [.env.example](.env.example)
- Debian/production: [apps/server/server.env.example](apps/server/server.env.example)

## Build Capture

### macOS

```bash
./deploy/build-mac.sh
open dist/SightRelay.app
```

This creates `dist/SightRelay.app` and `dist/SightRelay.dmg`. The build script copies the Cargo package version into `Info.plist`; update `crates/mac-capture/Cargo.toml` before publishing a new release.

### Windows

Run in Developer PowerShell for VS:

```powershell
.\deploy\build-windows.ps1
Start-Process .\dist\SightRelay-Windows\SightRelay.exe
```

## Runtime shortcuts

| Action | macOS | Windows |
| --- | --- | --- |
| Analyze one screenshot | `Option+A` | `Alt+A` |
| Append screenshot to group | `Option+S` | `Alt+S` |
| Submit the existing group | `Option+Z` | `Alt+Z` |
| Cancel and clear unsubmitted group | `Option+X` | `Alt+X` |

Group submission does not capture the current screen. Append the current page first when it belongs to the question. Mouse bindings are single-question only.

Capture configuration is stored at `~/Library/Application Support/SightRelay/config.env` on macOS and `%LOCALAPPDATA%\\SightRelay\\config.env` on Windows. macOS capture uses the primary display regardless of the compatibility display field; Windows uses the configured display ID. Automatic capture is disabled.

## Server configuration and storage

- `SIGHT_DATA_DIR`: SQLite database and local images; default `./data`.
- `SIGHT_PROMPT_FILE`: single-question prompt; default `.sight-prompt.md`.
- `SIGHT_MULTI_PAGE_PROMPT_FILE`: group prompt; default `.sight-multi-page-prompt.md`.
- `SIGHT_ANALYSIS_CONCURRENCY`: concurrent analysis jobs, clamped to `1..16`, default `2`.
- `AI_PROVIDER`: `openai`, `dashscope`, `deepseek`, `kimi`, `zhipu`, `minimax` or `other`.
- Each provider has independent base URL, proxy, model, reasoning/thinking options, extra JSON and API key. Admin changes are hot-applied.
- Without complete MinIO/S3 settings, images are stored under `${SIGHT_DATA_DIR}/captures/`; with them, image objects are stored in the configured bucket while SQLite remains the metadata store.
- A single uploaded image is limited to 10 MiB; the request limit also reserves multipart overhead.

Runtime databases, uploaded images, prompt copies, tokens and `.env` files must not be committed to a public repository.

## Web pages and roles

- `/`: authenticated responsive PC/mobile main page.
- `/settings`: settings page; administrators see users, LLM, usage and Capture devices, while regular users see only Capture devices.
- `/healthz`: health check.
- `/api/v1/ws`: browser and Capture WebSocket endpoint.

Captures, tasks, answers, devices and events are isolated per user. Single and group prompts are stored separately and new tasks use saved changes without restarting the Server.

## Production deployment

Debian:

```bash
sudo ./deploy/debian-install.sh
sudoedit /etc/sight-relay/server.env
sudo systemctl restart sight-relay
sudo journalctl -u sight-relay -f
```

Use `deploy/nginx.sight-relay.conf.example` for HTTPS and WebSocket proxying. Do not expose port 8080 directly to the public internet. `deploy/Dockerfile` and `deploy/docker-compose.yml` are provided as a containerization starting point; inject the Server environment variables explicitly for your deployment and persist `/data`.

## Verification

```bash
cargo test --workspace
cargo build --release -p sight-relay-server
./deploy/test-roi.sh       # macOS native ROI tests
```

## Security

Never commit `.env`, databases, uploaded images, device tokens, LLM API keys or MinIO credentials. Change `ADMIN_PASSWORD` before the first start, use HTTPS and least-privilege storage credentials in production, and avoid capturing passwords or other sensitive information.
