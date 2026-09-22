# macOS 捕获范围编辑器

在 Capture 设置中选择显示器，点击“设置捕获范围”。设置窗口隐藏，所选屏幕出现透明覆盖层：拖动四角/四边缩放，拖动框内移动，Enter 或“保存范围”确认，Esc 或“取消”保留原范围。“恢复全屏”只修改当前选框，需要确认才保存。

编辑期间屏蔽自动采集及截图快捷键，关闭覆盖层后恢复原采集状态。确认成功后 ROI 保存到用户的 SightRelay/config.env。覆盖层使用 AppKit 屏幕逻辑坐标，返回顶部为原点的归一化 ROI；Rust 对实际截图像素裁剪，避免 Retina 倍率造成偏移。当前范围属于所选单个显示器，不支持跨显示器选框。显示器布局改变时取消编辑。

开发运行前执行 `./deploy/build-roi.sh debug`，将 helper 放在 settings 可执行文件旁。完整打包使用 `./deploy/build-mac.sh`，自动把 helper 放入 App 的 Contents/MacOS，配置签名时先签 helper，再签 App。

验证：`./deploy/test-roi.sh`。包括几何边界、原生鼠标/键盘事件、透明度、返回协议校验。原生渲染预览输出到 `/tmp/sight-relay-roi-overlay.png`。自动测试不替代真实桌面、多显示器及不同缩放设置的人工验收。
