$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

Write-Host "Building portable Windows Capture client..."
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "未找到 cargo。请先安装 Rust，并确认 cargo 已加入 PATH。"
}
& (Join-Path $PSScriptRoot "create-windows-icon.ps1")
if (-not (Test-Path "assets\sight-relay.ico")) {
    throw "Windows 应用图标生成失败，未找到 assets\sight-relay.ico。"
}
cargo build --release -p mac-capture --bin settings --bin roi-overlay
if ($LASTEXITCODE -ne 0) {
    throw "Rust 编译失败，已停止打包，不会继续复制不存在的 Capture 客户端文件。"
}

$out = Join-Path (Get-Location) "dist\SightRelay-Windows"
$packageRoot = Join-Path (Get-Location) "dist\SightRelay-Windows-package"
$package = Join-Path $packageRoot "SightRelay-Windows"
if (Test-Path $packageRoot) { Remove-Item $packageRoot -Recurse -Force }
New-Item -ItemType Directory -Path $package | Out-Null
Copy-Item "target\release\settings.exe" (Join-Path $package "SightRelay.exe")
Copy-Item "target\release\roi-overlay.exe" (Join-Path $package "roi-overlay.exe")
Copy-Item "assets\sight-relay.ico" (Join-Path $package "sight-relay.ico")

@"
# Sight Relay Capture portable configuration
SERVER_URL=https://ot.sunyibud.online
DEVICE_TOKEN=replace-with-a-long-random-token
DEVICE_ID=windows-pc
INTERVAL_SECONDS=10
DISPLAY_ID=0
AUTO_CAPTURE=false
QUICK_CAPTURE_ENABLED=true
KEYBOARD_TRIGGER_ENABLED=true
MOUSE_TRIGGER_ENABLED=false
MOUSE_BUTTON=
MOUSE_TRIGGER_ACTION=single
ROI_X=0
ROI_Y=0
ROI_W=1
ROI_H=1
"@ | Set-Content (Join-Path $package "config.env.example") -Encoding UTF8

@"
Sight Relay Capture（Windows 绿色版）

双击 SightRelay.exe 即可运行，无需安装，不写注册表。
首次保存配置后，文件位于 %LOCALAPPDATA%\SightRelay\config.env。
配置示例 config.env.example 可用于准备 Server 地址、设备 ID 和 Token；运行后请在客户端内保存配置。
退出：托盘菜单选择“退出”，或按 Ctrl+Q。快捷键：Alt+A 截图上传。
可在“快捷触发”中录入鼠标按键，并选择单击、双击或长按触发；Windows 无需额外输入监控授权。
“设置捕获范围”会打开随包的 roi-overlay.exe；按 Enter 保存，Esc 取消。
Windows 10 需要安装 Microsoft Edge WebView2 Runtime；Windows 11 通常已内置。
"@ | Set-Content (Join-Path $package "README.txt") -Encoding UTF8

$zip = Join-Path (Get-Location) "dist\SightRelay-Windows-portable.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $package -DestinationPath $zip

try {
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }
    Copy-Item $package $out -Recurse
} catch {
    Write-Warning "已生成 ZIP，但运行中的客户端占用了 $out。请退出 SightRelay 后重新运行脚本以更新该目录。"
}
Write-Host "Created $zip"
