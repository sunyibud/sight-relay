#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(unused_imports, dead_code)]

use eframe::egui;
use global_hotkey::HotKeyState;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager,
    hotkey::{Code, HotKey, Modifiers},
};
use mac_capture::{Roi, capture_display, list_displays};
use std::sync::{Condvar, Mutex, OnceLock, mpsc};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const AUTOMATIC_CAPTURE_DISABLED_MESSAGE: &str = "管理员已关闭自动采集功能，建议通过快捷键手动采集";
const RELEASE_API_URL: &str =
    "https://git.sunyibud.online/api/v1/repos/sunyibud/sight-relay-release/releases/latest";
const RELEASE_PAGE_URL: &str = "https://git.sunyibud.online/sunyibud/sight-relay-release/releases";
const RELEASE_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, serde::Deserialize)]
struct GiteaLatestRelease {
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum UpdateCheckResult {
    Available {
        current_version: String,
        latest_version: String,
        notes: String,
    },
    UpToDate {
        current_version: String,
    },
    Failed,
}

fn release_version(value: &str) -> Option<semver::Version> {
    semver::Version::parse(value.trim().trim_start_matches(['v', 'V'])).ok()
}

fn is_newer_release(current: &str, remote_tag: &str) -> bool {
    match (release_version(current), release_version(remote_tag)) {
        (Some(current), Some(remote)) => remote > current,
        _ => false,
    }
}

fn release_notes(body: &str) -> String {
    let normalized = body.trim();
    let mut notes = normalized.chars().take(320).collect::<String>();
    if normalized.chars().count() > 320 {
        notes.push('…');
    }
    notes
}

fn latest_release_result() -> UpdateCheckResult {
    let current_version = APP_VERSION.to_string();
    let client = match reqwest::blocking::Client::builder()
        .timeout(RELEASE_CHECK_TIMEOUT)
        .user_agent(format!("SightRelay-Capture/{APP_VERSION}"))
        .build()
    {
        Ok(client) => client,
        Err(_) => return UpdateCheckResult::Failed,
    };
    let release = match client
        .get(RELEASE_API_URL)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .ok()
        .and_then(|body| serde_json::from_str::<GiteaLatestRelease>(&body).ok())
    {
        Some(release) => release,
        None => return UpdateCheckResult::Failed,
    };
    if release.draft || release.prerelease || !is_newer_release(&current_version, &release.tag_name)
    {
        return UpdateCheckResult::UpToDate { current_version };
    }
    let Some(latest_version) = release_version(&release.tag_name) else {
        return UpdateCheckResult::Failed;
    };
    UpdateCheckResult::Available {
        current_version,
        latest_version: latest_version.to_string(),
        notes: release_notes(&release.body),
    }
}

fn start_update_check(sender: mpsc::Sender<UpdateCheckResult>, in_flight: Arc<AtomicBool>) {
    if in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    thread::spawn(move || {
        let result = latest_release_result();
        let _ = sender.send(result);
        in_flight.store(false, Ordering::Release);
    });
}

fn open_release_page() {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(RELEASE_PAGE_URL).spawn();
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd")
        .args(["/C", "start", "", RELEASE_PAGE_URL])
        .spawn();
}

fn automatic_capture_disabled_message() -> &'static str {
    AUTOMATIC_CAPTURE_DISABLED_MESSAGE
}

fn enforce_manual_capture(config: &mut Config) {
    config.auto_capture = false;
}

#[derive(serde::Serialize)]
struct PlatformPresentation {
    capture_key: &'static str,
    capture_shortcut: &'static str,
    quit_shortcut: &'static str,
    mouse_trigger_supported: bool,
}

fn platform_presentation() -> PlatformPresentation {
    #[cfg(target_os = "windows")]
    {
        return PlatformPresentation {
            capture_key: "Alt",
            capture_shortcut: "Alt+A",
            quit_shortcut: "Ctrl+Q",
            mouse_trigger_supported: true,
        };
    }
    #[cfg(not(target_os = "windows"))]
    PlatformPresentation {
        capture_key: "Option",
        capture_shortcut: "Option+A",
        quit_shortcut: "Command+Q",
        mouse_trigger_supported: cfg!(target_os = "macos"),
    }
}

fn quit_hotkey() -> HotKey {
    #[cfg(target_os = "windows")]
    {
        return HotKey::new(Some(Modifiers::CONTROL), Code::KeyQ);
    }
    #[cfg(not(target_os = "windows"))]
    HotKey::new(Some(Modifiers::META), Code::KeyQ)
}

fn windows_config_path(local_app_data: Option<PathBuf>) -> PathBuf {
    local_app_data
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from("."))
        })
        .join("SightRelay")
        .join("config.env")
}

fn roi_helper_path(executable: &Path) -> PathBuf {
    let file_name = if cfg!(target_os = "windows") {
        "roi-overlay.exe"
    } else {
        "roi-overlay"
    };
    executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(file_name)
}

fn selected_display(current: u32, displays: &[(u32, String, u32, u32)]) -> u32 {
    displays
        .iter()
        .find(|(id, ..)| *id == current)
        .map(|(id, ..)| *id)
        .or_else(|| displays.first().map(|(id, ..)| *id))
        .unwrap_or(current)
}

fn default_device_id() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        return "windows-pc";
    }
    #[cfg(not(target_os = "windows"))]
    "mac-m5"
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod web_ui {
    use super::{
        Config, MouseMonitorEvent, MouseTriggerAction, MouseTriggerRecognizer, Roi,
        WebsocketReconnectSignal, apply_runtime_config, capture_display, default_mouse_action,
        enforce_manual_capture, mouse_button_label, mouse_monitor, open_release_page, save_config,
        start_update_check, upload,
    };
    use global_hotkey::{
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
        hotkey::{Code, HotKey, Modifiers},
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::{fs, process::Command};
    use tao::{
        event::Event,
        event_loop::{ControlFlow, EventLoop},
        window::{Icon as WindowIcon, WindowBuilder},
    };
    use tray_icon::{
        Icon, TrayIconBuilder,
        menu::{Menu, MenuEvent, MenuItem},
    };
    use wry::WebViewBuilder;

    enum MouseUiCommand {
        StartBinding,
        CancelBinding,
        StartTest {
            button: u32,
            action: MouseTriggerAction,
        },
        EnsureMonitor,
        StopMonitor,
    }

    fn glass_tray_icon(connected: bool) -> Icon {
        let size = 32u32;
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 - 15.5;
                let dy = y as f32 - 15.5;
                let distance = (dx * dx + dy * dy).sqrt();
                let edge = ((16.0 - distance) * 5.0).clamp(0.0, 1.0);
                let highlight = (1.0 - ((x as f32 + y as f32) / 64.0)).clamp(0.0, 1.0);
                let (r, g, b) = if connected {
                    (78, 211, 157)
                } else {
                    (224, 82, 103)
                };
                rgba.extend_from_slice(&[r, g, b, (edge * (190.0 + highlight * 55.0)) as u8]);
            }
        }
        Icon::from_rgba(rgba, size, size).expect("tray icon")
    }

    pub(super) fn application_window_icon() -> WindowIcon {
        let size = 32u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        for y in 0..size {
            for x in 0..size {
                let offset = ((y * size + x) * 4) as usize;
                rgba[offset..offset + 4].copy_from_slice(&[65, 125, 226, 255]);
            }
        }
        let inset = 8u32;
        let arm = 7u32;
        let thickness = 2u32;
        for y in 0..size {
            for x in 0..size {
                let corner = (x >= inset && x < inset + arm && y >= inset && y < inset + thickness)
                    || (x >= inset && x < inset + thickness && y >= inset && y < inset + arm)
                    || (x >= size - inset - arm
                        && x < size - inset
                        && y >= inset
                        && y < inset + thickness)
                    || (x >= size - inset - thickness
                        && x < size - inset
                        && y >= inset
                        && y < inset + arm)
                    || (x >= inset
                        && x < inset + arm
                        && y >= size - inset - thickness
                        && y < size - inset)
                    || (x >= inset
                        && x < inset + thickness
                        && y >= size - inset - arm
                        && y < size - inset)
                    || (x >= size - inset - arm
                        && x < size - inset
                        && y >= size - inset - thickness
                        && y < size - inset)
                    || (x >= size - inset - thickness
                        && x < size - inset
                        && y >= size - inset - arm
                        && y < size - inset);
                if corner {
                    let offset = ((y * size + x) * 4) as usize;
                    rgba[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        WindowIcon::from_rgba(rgba, size, size).expect("window icon")
    }

    pub fn run(
        config: Arc<Mutex<Config>>,
        active: Arc<AtomicBool>,
        gate: Arc<Mutex<bool>>,
        ws_connected: Arc<AtomicBool>,
        websocket_reconnect: Arc<WebsocketReconnectSignal>,
    ) {
        let event_loop = EventLoop::new();
        let window = WindowBuilder::new()
            .with_title("Sight Relay")
            .with_window_icon(Some(application_window_icon()))
            .with_inner_size(tao::dpi::LogicalSize::new(1180.0, 760.0))
            .with_min_inner_size(tao::dpi::LogicalSize::new(980.0, 620.0))
            .with_resizable(true)
            .build(&event_loop)
            .expect("failed to create window");
        let hotkey_manager = GlobalHotKeyManager::new().ok();
        let quit_hotkey = super::quit_hotkey();
        let capture_hotkey = HotKey::new(Some(Modifiers::ALT), Code::KeyA);
        let multi_append_hotkey = HotKey::new(Some(Modifiers::ALT), Code::KeyS);
        let multi_submit_hotkey = HotKey::new(Some(Modifiers::ALT), Code::KeyZ);
        let multi_cancel_hotkey = HotKey::new(Some(Modifiers::ALT), Code::KeyX);
        if let Some(manager) = &hotkey_manager {
            if let Err(error) = manager.register(quit_hotkey) {
                eprintln!(
                    "failed to register {}: {error}",
                    super::platform_presentation().quit_shortcut
                );
            }
            if let Err(error) = manager.register(capture_hotkey) {
                eprintln!(
                    "failed to register {}: {error}",
                    super::platform_presentation().capture_shortcut
                );
            }
            for (hotkey, label) in [
                (multi_append_hotkey, "Option+S"),
                (multi_submit_hotkey, "Option+Z"),
                (multi_cancel_hotkey, "Option+X"),
            ] {
                if let Err(error) = manager.register(hotkey) {
                    eprintln!("failed to register {label}: {error}");
                }
            }
        }
        let tray_menu = Menu::new();
        let show_item = MenuItem::with_id("show", "显示 Sight Relay", true, None);
        let shot_item = MenuItem::with_id("shot", "立即截图", true, None);
        let roi_item = MenuItem::with_id("roi", "设置捕获范围", true, None);
        let quit_item = MenuItem::with_id("quit", "退出", true, None);
        let _ = tray_menu.append(&show_item);
        let _ = tray_menu.append(&shot_item);
        let _ = tray_menu.append(&roi_item);
        let _ = tray_menu.append(&quit_item);
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("Sight Relay Capture")
            .with_icon(glass_tray_icon(ws_connected.load(Ordering::Relaxed)))
            .build()
            .expect("failed to create menu bar icon");
        let initial = config.lock().unwrap().clone();
        let (mouse_monitor, mouse_events) = mouse_monitor();
        if initial.quick_capture_enabled && initial.mouse_trigger_enabled {
            mouse_monitor.ensure_running(true);
        }
        let initial_json = serde_json::to_string(&initial).unwrap_or_else(|_| "{}".into());
        let displays_json = serde_json::to_string(&super::list_displays().unwrap_or_default())
            .unwrap_or_else(|_| "[]".into());
        let platform_json =
            serde_json::to_string(&super::platform_presentation()).unwrap_or_else(|_| "{}".into());
        let html = include_str!("../../../../web/capture-client.html")
            .replace("http://127.0.0.1:8080", &initial.server)
            .replace("value=\"mac-m5\"", &format!("value=\"{}\"", initial.device))
            .replace(
                "<script>",
                &format!(
                    "<script>window.__INITIAL_CONFIG__={initial_json};window.__DISPLAYS__={displays_json};window.__PLATFORM__={platform_json};"
                ),
            );
        let config_for_ipc = config.clone();
        let reconnect_for_ipc = websocket_reconnect.clone();
        let active_for_ipc = active.clone();
        let gate_for_ipc = gate.clone();
        let ws_state_for_ipc = ws_connected.clone();
        let (clipboard_tx, clipboard_rx) = std::sync::mpsc::channel::<()>();
        let (mouse_command_tx, mouse_command_rx) = std::sync::mpsc::channel::<MouseUiCommand>();
        let (update_tx, update_rx) = std::sync::mpsc::channel::<super::UpdateCheckResult>();
        let update_in_flight = Arc::new(AtomicBool::new(false));
        let webview = WebViewBuilder::new()
            .with_html(html)
            .with_devtools(false)
            .with_ipc_handler(move |request| {
                let Ok(message) = serde_json::from_str::<serde_json::Value>(request.body()) else {
                    return;
                };
                let kind = message
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let mut c = config_for_ipc.lock().unwrap().clone();
                match kind {
                    "toggle_capture" => {
                        // Automatic capture is intentionally disabled by the administrator.
                        // Manual captures continue to use the Option-A shortcut or test button.
                        active_for_ipc.store(false, Ordering::Relaxed);
                        enforce_manual_capture(&mut c);
                        let _ = save_config(&c);
                    }
                    "set_connection_state" => {
                        if let Some(connected) = message.get("connected").and_then(|v| v.as_bool())
                        {
                            ws_state_for_ipc.store(connected, Ordering::Relaxed);
                        }
                    }
                    "paste_token" => {
                        let _ = clipboard_tx.send(());
                    }
                    "open_release_page" => {
                        open_release_page();
                    }
                    "start_mouse_binding" => {
                        let _ = mouse_command_tx.send(MouseUiCommand::StartBinding);
                    }
                    "cancel_mouse_binding" => {
                        let _ = mouse_command_tx.send(MouseUiCommand::CancelBinding);
                    }
                    "test_mouse_trigger" => {
                        if let Some(button) = message.get("mouse_button").and_then(|v| v.as_u64()) {
                            let action = message
                                .get("mouse_trigger_action")
                                .and_then(|v| v.as_str())
                                .map(MouseTriggerAction::parse)
                                .unwrap_or_default();
                            let _ = mouse_command_tx.send(MouseUiCommand::StartTest {
                                button: button as u32,
                                action,
                            });
                        }
                    }
                    "clear_mouse_binding" => {
                        c.mouse_button = None;
                        c.mouse_trigger_enabled = false;
                        let _ = save_config(&c);
                        apply_runtime_config(&config_for_ipc, c, &reconnect_for_ipc);
                        let _ = mouse_command_tx.send(MouseUiCommand::StopMonitor);
                    }
                    "test_screenshot" => {
                        let preview_path = std::env::temp_dir()
                            .join(format!("sight-relay-capture-{}.jpg", std::process::id()));
                        if let Ok(image) = capture_display(c.display, c.roi) {
                            let _ = std::fs::write(&preview_path, &image.bytes);
                            #[cfg(target_os = "macos")]
                            let _ = std::process::Command::new("open")
                                .arg(&preview_path)
                                .spawn();
                            #[cfg(target_os = "windows")]
                            let _ = std::process::Command::new("cmd")
                                .arg("/C")
                                .arg("start")
                                .arg("")
                                .arg(&preview_path)
                                .spawn();
                        }
                        upload(&c, &gate_for_ipc);
                    }
                    "open_roi" => {
                        let config_for_roi = config_for_ipc.clone();
                        let gate_for_roi = gate_for_ipc.clone();
                        std::thread::spawn(move || {
                            let Some(helper) = std::env::current_exe()
                                .ok()
                                .map(|p| super::roi_helper_path(&p))
                                .filter(|p| p.is_file())
                            else {
                                return;
                            };
                            *gate_for_roi.lock().unwrap() = true;
                            let preview = std::env::temp_dir().join(format!(
                                "sight-relay-web-preview-{}.jpg",
                                std::process::id()
                            ));
                            if let Ok(image) = capture_display(c.display, Roi::full()) {
                                let _ = fs::write(&preview, image.bytes);
                            }
                            let output = Command::new(helper)
                                .args([
                                    c.display.to_string(),
                                    preview.to_string_lossy().to_string(),
                                    c.roi.x.to_string(),
                                    c.roi.y.to_string(),
                                    c.roi.width.to_string(),
                                    c.roi.height.to_string(),
                                ])
                                .output();
                            if let Ok(out) = output
                                && out.status.success()
                                && let Ok(roi) = serde_json::from_slice::<Roi>(&out.stdout)
                            {
                                let mut updated = config_for_roi.lock().unwrap().clone();
                                updated.roi = roi;
                                let _ = save_config(&updated);
                                *config_for_roi.lock().unwrap() = updated;
                            }
                            let _ = fs::remove_file(preview);
                            *gate_for_roi.lock().unwrap() = false;
                        });
                    }
                    "save_config" => {
                        if let Some(v) = message.get("server").and_then(|v| v.as_str()) {
                            c.server = v.into();
                        }
                        if let Some(v) = message.get("token").and_then(|v| v.as_str()) {
                            c.token = v.into();
                        }
                        if let Some(v) = message.get("device").and_then(|v| v.as_str()) {
                            c.device = v.into();
                        }
                        if let Some(v) = message.get("interval").and_then(|v| v.as_u64()) {
                            c.interval = v.max(1);
                        }
                        if let Some(v) = message
                            .get("display")
                            .and_then(|v| v.as_str())
                            .and_then(|v| v.parse().ok())
                        {
                            c.display = v;
                        }
                        if let Some(v) = message
                            .get("quick_capture_enabled")
                            .and_then(|v| v.as_bool())
                        {
                            c.quick_capture_enabled = v;
                        }
                        if let Some(v) = message
                            .get("keyboard_trigger_enabled")
                            .and_then(|v| v.as_bool())
                        {
                            c.keyboard_trigger_enabled = v;
                        }
                        if let Some(v) = message
                            .get("mouse_trigger_enabled")
                            .and_then(|v| v.as_bool())
                        {
                            c.mouse_trigger_enabled = v;
                        }
                        if message.get("mouse_button").is_some() {
                            c.mouse_button = message
                                .get("mouse_button")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                        }
                        if let Some(v) =
                            message.get("mouse_trigger_action").and_then(|v| v.as_str())
                        {
                            c.mouse_trigger_action = MouseTriggerAction::parse(v);
                        }
                        enforce_manual_capture(&mut c);
                        active_for_ipc.store(false, Ordering::Relaxed);
                        let _ = save_config(&c);
                        if apply_runtime_config(&config_for_ipc, c, &reconnect_for_ipc) {
                            ws_state_for_ipc.store(false, Ordering::Relaxed);
                        }
                        let updated = config_for_ipc.lock().unwrap();
                        if updated.quick_capture_enabled && updated.mouse_trigger_enabled {
                            let _ = mouse_command_tx.send(MouseUiCommand::EnsureMonitor);
                        } else {
                            let _ = mouse_command_tx.send(MouseUiCommand::StopMonitor);
                        }
                    }
                    _ => {}
                }
            })
            .build(&window)
            .expect("failed to create WebView");
        start_update_check(update_tx, update_in_flight);
        let mut tray_state = ws_connected.load(Ordering::Relaxed);
        let mut mouse_recognizer = MouseTriggerRecognizer::default();
        let mouse_upload_in_flight = Arc::new(AtomicBool::new(false));
        let mut binding_active = false;
        let mut testing_active = false;
        let mut testing_config: Option<Config> = None;
        let mut mouse_deadline: Option<std::time::Instant> = None;
        let mut ignore_mouse_until: Option<std::time::Instant> = None;
        let mut suppressed_button: Option<u32> = None;
        event_loop.run(move |event, _, control_flow| {
            let _ = &hotkey_manager;
            while let Ok(result) = update_rx.try_recv() {
                let payload = serde_json::to_string(&result)
                    .unwrap_or_else(|_| "{\"status\":\"failed\"}".into());
                let _ = webview.evaluate_script(&format!(
                    "window.__updateCheckResult({payload})"
                ));
            }
            while clipboard_rx.try_recv().is_ok() {
                let script = match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                    Ok(text) => format!(
                        "window.__applyTokenPaste({})",
                        serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into())
                    ),
                    Err(error) => format!(
                        "window.__tokenPasteFailed({})",
                        serde_json::to_string(&error.to_string())
                            .unwrap_or_else(|_| "\"clipboard error\"".into())
                    ),
                };
                let _ = webview.evaluate_script(&script);
            }
            while let Ok(command) = mouse_command_rx.try_recv() {
                match command {
                    MouseUiCommand::StartBinding => {
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        {
                            mouse_monitor.ensure_running(true);
                            binding_active = true;
                            testing_active = false;
                            testing_config = None;
                            mouse_deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
                            ignore_mouse_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
                            mouse_recognizer.reset();
                        }
                        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                        let _ = webview.evaluate_script("window.__mouseBindingFailed('当前平台暂不支持鼠标按键触发')");
                    }
                    MouseUiCommand::CancelBinding => {
                        binding_active = false;
                        testing_active = false;
                        testing_config = None;
                        mouse_deadline = None;
                        mouse_recognizer.reset();
                    }
                    MouseUiCommand::StartTest { button, action } => {
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        {
                            mouse_monitor.ensure_running(true);
                            testing_active = true;
                            let mut temporary = config.lock().unwrap().clone();
                            temporary.quick_capture_enabled = true;
                            temporary.mouse_trigger_enabled = true;
                            temporary.mouse_button = Some(button);
                            temporary.mouse_trigger_action = action;
                            testing_config = Some(temporary);
                            binding_active = false;
                            mouse_deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
                            ignore_mouse_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
                            mouse_recognizer.reset();
                        }
                        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                        let _ = webview.evaluate_script("window.__mouseBindingFailed('当前平台暂不支持鼠标按键触发')");
                    }
                    MouseUiCommand::EnsureMonitor => mouse_monitor.ensure_running(true),
                    MouseUiCommand::StopMonitor => {
                        mouse_monitor.stop();
                        mouse_recognizer.reset();
                    }
                }
            }
            while let Ok(mouse_event) = mouse_events.try_recv() {
                match mouse_event {
                    MouseMonitorEvent::Ready => {
                        let _ = webview.evaluate_script("window.__mouseMonitorReady?.()");
                    }
                    MouseMonitorEvent::PermissionDenied => {
                        binding_active = false;
                        testing_active = false;
                        testing_config = None;
                        mouse_deadline = None;
                        let _ = webview.evaluate_script("window.__mouseBindingFailed('未获得输入监控权限，请前往“系统设置 → 隐私与安全性 → 输入监控”允许 Sight Relay')");
                    }
                    MouseMonitorEvent::Unavailable => {
                        binding_active = false;
                        testing_active = false;
                        testing_config = None;
                        mouse_deadline = None;
                        let script = format!(
                            "window.__mouseBindingFailed({})",
                            serde_json::to_string(super::mouse_monitor_unavailable_message())
                                .unwrap()
                        );
                        let _ = webview.evaluate_script(&script);
                    }
                    MouseMonitorEvent::Input(input) => {
                        if ignore_mouse_until.is_some_and(|until| std::time::Instant::now() < until) {
                            continue;
                        }
                        if suppressed_button == Some(input.button) {
                            if input.phase == super::MousePhase::Up {
                                suppressed_button = None;
                            }
                            continue;
                        }
                        if binding_active && input.phase == super::MousePhase::Down {
                            binding_active = false;
                            mouse_deadline = None;
                            suppressed_button = Some(input.button);
                            let action = default_mouse_action(input.button);
                            let script = format!(
                                "window.__mouseBindingDetected?.({{button:{},label:{},recommendedAction:{}}})",
                                input.button,
                                serde_json::to_string(&mouse_button_label(input.button)).unwrap(),
                                serde_json::to_string(action.as_env()).unwrap(),
                            );
                            let _ = webview.evaluate_script(&script);
                            continue;
                        }
                        let current = testing_config
                            .clone()
                            .unwrap_or_else(|| config.lock().unwrap().clone());
                        let editing = *gate.lock().unwrap();
                        if mouse_recognizer.handle(input, &current, editing) {
                            eprintln!("mouse capture trigger activated: {}", mouse_button_label(input.button));
                            if super::begin_mouse_upload(&mouse_upload_in_flight) {
                                let upload_gate = gate.clone();
                                let upload_in_flight = mouse_upload_in_flight.clone();
                                std::thread::spawn(move || {
                                    upload(&current, &upload_gate);
                                    upload_in_flight.store(false, Ordering::Release);
                                });
                            } else {
                                eprintln!("mouse capture ignored while a previous upload is in flight");
                            }
                            if testing_active {
                                testing_active = false;
                                testing_config = None;
                                mouse_deadline = None;
                                let _ = webview.evaluate_script("window.__mouseTriggerTested?.()");
                            }
                        }
                    }
                }
            }
            if mouse_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                binding_active = false;
                testing_active = false;
                testing_config = None;
                mouse_deadline = None;
                let _ = webview.evaluate_script("window.__mouseBindingTimedOut?.()");
            }
            let current_state = ws_connected.load(Ordering::Relaxed);
            if current_state != tray_state {
                let _ = tray_icon.set_icon(Some(glass_tray_icon(current_state)));
                tray_state = current_state;
            }
            *control_flow = ControlFlow::Poll;
            if let Ok(hotkey) = GlobalHotKeyEvent::receiver().try_recv() {
                if hotkey.id == quit_hotkey.id() && hotkey.state == HotKeyState::Pressed {
                    std::process::exit(0);
                }
                if hotkey.state == HotKeyState::Pressed
                    && hotkey.id == multi_append_hotkey.id()
                {
                    let c = config.lock().unwrap().clone();
                    super::upload_action(&c, &gate, super::UploadAction::MultiAppend);
                }
                if hotkey.state == HotKeyState::Pressed
                    && hotkey.id == multi_submit_hotkey.id()
                {
                    let c = config.lock().unwrap().clone();
                    super::upload_action(&c, &gate, super::UploadAction::MultiSubmit);
                }
                if hotkey.state == HotKeyState::Pressed
                    && hotkey.id == multi_cancel_hotkey.id()
                {
                    let c = config.lock().unwrap().clone();
                    super::upload_action(&c, &gate, super::UploadAction::MultiCancel);
                }
                if hotkey.id == capture_hotkey.id() && hotkey.state == HotKeyState::Pressed {
                    eprintln!(
                        "{} screenshot shortcut triggered",
                        super::platform_presentation().capture_shortcut
                    );
                    let c = config.lock().unwrap().clone();
                    if c.quick_capture_enabled && c.keyboard_trigger_enabled && !*gate.lock().unwrap() {
                        upload(&c, &gate);
                    }
                }
            }
            if let Ok(menu_event) = MenuEvent::receiver().try_recv() {
                if menu_event.id == show_item.id() {
                    window.set_visible(true);
                    window.set_focus();
                }
                if menu_event.id == shot_item.id() {
                    let c = config.lock().unwrap().clone();
                    upload(&c, &gate);
                }
                if menu_event.id == roi_item.id() {
                    super::launch_roi_editor(config.clone(), gate.clone());
                }
                if menu_event.id == quit_item.id() {
                    std::process::exit(0);
                }
            }
            if let Event::WindowEvent {
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } = event
            {
                window.set_visible(false);
            }
        });
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn launch_roi_editor(config: Arc<Mutex<Config>>, gate: Arc<Mutex<bool>>) {
    let helper = std::env::current_exe()
        .ok()
        .map(|path| roi_helper_path(&path));
    let Some(helper) = helper.filter(|p| p.is_file()) else {
        eprintln!("未找到范围编辑器 roi-overlay");
        return;
    };
    if *gate.lock().unwrap() {
        return;
    }
    *gate.lock().unwrap() = true;
    let c = config.lock().unwrap().clone();
    let preview = std::env::temp_dir().join(format!(
        "sight-relay-tray-preview-{}.jpg",
        std::process::id()
    ));
    let preview_arg = preview.to_string_lossy().to_string();
    let Ok(image) = capture_display(c.display, Roi::full()) else {
        *gate.lock().unwrap() = false;
        return;
    };
    if fs::write(&preview, image.bytes).is_err() {
        *gate.lock().unwrap() = false;
        return;
    }
    thread::spawn(move || {
        let result = std::process::Command::new(helper)
            .args([
                c.display.to_string(),
                preview_arg,
                c.roi.x.to_string(),
                c.roi.y.to_string(),
                c.roi.width.to_string(),
                c.roi.height.to_string(),
            ])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    parse_roi_result(&o.stdout).ok().flatten()
                } else {
                    None
                }
            });
        let _ = fs::remove_file(&preview);
        if let Some(roi) = result {
            let mut next = c;
            next.roi = roi;
            let _ = save_config(&next);
            *config.lock().unwrap() = next;
        }
        *gate.lock().unwrap() = false;
    });
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum MouseTriggerAction {
    #[default]
    Single,
    Double,
    LongPress,
}

impl MouseTriggerAction {
    fn as_env(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Double => "double",
            Self::LongPress => "long_press",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "double" => Self::Double,
            "long_press" => Self::LongPress,
            _ => Self::Single,
        }
    }
}

fn default_mouse_action(button: u32) -> MouseTriggerAction {
    if button <= 1 {
        MouseTriggerAction::Double
    } else {
        MouseTriggerAction::Single
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MousePhase {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseInputEvent {
    button: u32,
    phase: MousePhase,
    timestamp_ms: u64,
}

impl MouseInputEvent {
    #[cfg(test)]
    fn up(button: u32, timestamp_ms: u64) -> Self {
        Self {
            button,
            phase: MousePhase::Up,
            timestamp_ms,
        }
    }
}

#[derive(Default)]
struct MouseTriggerRecognizer {
    pressed_at: HashMap<u32, u64>,
    last_click_at: HashMap<u32, u64>,
}

impl MouseTriggerRecognizer {
    const DOUBLE_CLICK_MS: u64 = 500;
    const LONG_PRESS_MS: u64 = 700;

    fn reset(&mut self) {
        self.pressed_at.clear();
        self.last_click_at.clear();
    }

    fn handle(&mut self, event: MouseInputEvent, config: &Config, roi_editing: bool) -> bool {
        if roi_editing
            || !config.quick_capture_enabled
            || !config.mouse_trigger_enabled
            || config.mouse_button != Some(event.button)
        {
            self.reset();
            return false;
        }

        match (config.mouse_trigger_action, event.phase) {
            (MouseTriggerAction::Single, MousePhase::Up) => true,
            (MouseTriggerAction::Double, MousePhase::Up) => {
                let previous = self.last_click_at.insert(event.button, event.timestamp_ms);
                if previous.is_some_and(|at| {
                    event.timestamp_ms.saturating_sub(at) <= Self::DOUBLE_CLICK_MS
                }) {
                    self.last_click_at.remove(&event.button);
                    true
                } else {
                    false
                }
            }
            (MouseTriggerAction::LongPress, MousePhase::Down) => {
                self.pressed_at.insert(event.button, event.timestamp_ms);
                false
            }
            (MouseTriggerAction::LongPress, MousePhase::Up) => self
                .pressed_at
                .remove(&event.button)
                .is_some_and(|at| event.timestamp_ms.saturating_sub(at) >= Self::LONG_PRESS_MS),
            _ => false,
        }
    }
}

fn begin_mouse_upload(in_flight: &AtomicBool) -> bool {
    in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn mouse_button_label(button: u32) -> String {
    match button {
        0 => "鼠标左键".into(),
        1 => "鼠标右键".into(),
        2 => "鼠标中键".into(),
        value => format!("鼠标侧键 {}", value + 1),
    }
}

#[derive(Debug)]
enum MouseMonitorEvent {
    Ready,
    PermissionDenied,
    Unavailable,
    Input(MouseInputEvent),
}

fn mouse_monitor_unavailable_message() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        return "无法启动鼠标监听，请关闭冲突的鼠标工具后重试";
    }
    #[cfg(not(target_os = "windows"))]
    "无法启动鼠标监听，请检查输入监控权限后重试"
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct MouseMonitorStarter {
    state: Arc<std::sync::atomic::AtomicU8>,
    desired: Arc<AtomicBool>,
    events: std::sync::mpsc::Sender<MouseMonitorEvent>,
}

#[cfg(target_os = "windows")]
#[derive(Clone)]
struct MouseMonitorStarter {
    state: Arc<std::sync::atomic::AtomicU8>,
    desired: Arc<AtomicBool>,
    thread_id: Arc<std::sync::atomic::AtomicU32>,
    events: std::sync::mpsc::Sender<MouseMonitorEvent>,
}

#[cfg(target_os = "windows")]
impl MouseMonitorStarter {
    fn ensure_running(&self, _request_permission: bool) {
        use std::sync::atomic::AtomicU8;

        let was_desired = self.desired.swap(true, Ordering::AcqRel);
        if self.state.load(Ordering::Acquire) == 2 {
            if !was_desired {
                let restart = self.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(250));
                    restart.ensure_running(false);
                });
            }
            let _ = self.events.send(MouseMonitorEvent::Ready);
            return;
        }
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let state = self.state.clone();
        let desired = self.desired.clone();
        let thread_id = self.thread_id.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            use windows_sys::Win32::{
                System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
                UI::WindowsAndMessaging::{
                    DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW,
                    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL,
                },
            };

            thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
            let module = unsafe { GetModuleHandleW(std::ptr::null()) };
            let hook =
                unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(windows_mouse_hook_proc), module, 0) };
            if hook.is_null() {
                thread_id.store(0, Ordering::Release);
                state.store(0, Ordering::Release);
                let _ = events.send(MouseMonitorEvent::Unavailable);
                return;
            }
            if !desired.load(Ordering::Acquire) {
                unsafe { UnhookWindowsHookEx(hook) };
                thread_id.store(0, Ordering::Release);
                state.store(0, Ordering::Release);
                return;
            }

            let callback = windows_mouse_hook_callback();
            *callback.lock().unwrap() = Some(WindowsMouseHookCallback {
                desired: desired.clone(),
                events: events.clone(),
                started: Instant::now(),
            });

            let mut message: MSG = unsafe { std::mem::zeroed() };
            unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
            state.store(2, Ordering::Release);
            let _ = events.send(MouseMonitorEvent::Ready);
            while desired.load(Ordering::Acquire)
                && unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0
            {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }

            unsafe { UnhookWindowsHookEx(hook) };
            *callback.lock().unwrap() = None;
            thread_id.store(0, Ordering::Release);
            state.store(0, Ordering::Release);
        });
    }

    fn stop(&self) {
        use windows_sys::Win32::{
            Foundation::{LPARAM, WPARAM},
            UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT},
        };

        self.desired.store(false, Ordering::Release);
        let thread_id = self.thread_id.load(Ordering::Acquire);
        if thread_id != 0 {
            unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0 as WPARAM, 0 as LPARAM) };
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Clone)]
struct MouseMonitorStarter;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl MouseMonitorStarter {
    fn ensure_running(&self, _request_permission: bool) {}
    fn stop(&self) {}
}

#[cfg(target_os = "macos")]
impl MouseMonitorStarter {
    fn ensure_running(&self, request_permission: bool) {
        use std::sync::atomic::AtomicU8;

        let was_desired = self.desired.swap(true, Ordering::AcqRel);
        if self.state.load(Ordering::Acquire) == 2 {
            if !was_desired {
                let restart = self.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(250));
                    restart.ensure_running(false);
                });
            }
            let _ = self.events.send(MouseMonitorEvent::Ready);
            return;
        }
        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let state = self.state.clone();
        let desired = self.desired.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            if !mac_mouse_event_access(request_permission) {
                state.store(0, Ordering::Release);
                let _ = events.send(MouseMonitorEvent::PermissionDenied);
                return;
            }
            if !desired.load(Ordering::Acquire) {
                state.store(0, Ordering::Release);
                return;
            }

            use core_foundation::runloop::{
                CFRunLoop, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
            };
            use core_graphics::event::{
                CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
                CGEventType, EventField,
            };

            let started = Instant::now();
            let callback_events = events.clone();
            let callback_desired = desired.clone();
            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                vec![
                    CGEventType::LeftMouseDown,
                    CGEventType::LeftMouseUp,
                    CGEventType::RightMouseDown,
                    CGEventType::RightMouseUp,
                    CGEventType::OtherMouseDown,
                    CGEventType::OtherMouseUp,
                ],
                move |_, event_type, event| {
                    if !callback_desired.load(Ordering::Acquire) {
                        return None;
                    }
                    let phase = match event_type {
                        CGEventType::LeftMouseDown
                        | CGEventType::RightMouseDown
                        | CGEventType::OtherMouseDown => MousePhase::Down,
                        CGEventType::LeftMouseUp
                        | CGEventType::RightMouseUp
                        | CGEventType::OtherMouseUp => MousePhase::Up,
                        _ => return None,
                    };
                    let button = match event_type {
                        CGEventType::LeftMouseDown | CGEventType::LeftMouseUp => 0,
                        CGEventType::RightMouseDown | CGEventType::RightMouseUp => 1,
                        _ => event
                            .get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER)
                            .max(0) as u32,
                    };
                    let _ = callback_events.send(MouseMonitorEvent::Input(MouseInputEvent {
                        button,
                        phase,
                        timestamp_ms: started.elapsed().as_millis() as u64,
                    }));
                    None
                },
            );
            let Ok(tap) = tap else {
                state.store(0, Ordering::Release);
                let _ = events.send(MouseMonitorEvent::Unavailable);
                return;
            };
            let run_loop = CFRunLoop::get_current();
            let Ok(source) = tap.mach_port.create_runloop_source(0) else {
                state.store(0, Ordering::Release);
                let _ = events.send(MouseMonitorEvent::Unavailable);
                return;
            };
            unsafe { run_loop.add_source(&source, kCFRunLoopCommonModes) };
            tap.enable();
            state.store(2, Ordering::Release);
            let _ = events.send(MouseMonitorEvent::Ready);
            while desired.load(Ordering::Acquire) {
                CFRunLoop::run_in_mode(
                    unsafe { kCFRunLoopDefaultMode },
                    Duration::from_millis(200),
                    false,
                );
            }
            state.store(0, Ordering::Release);
        });
    }

    fn stop(&self) {
        self.desired.store(false, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
fn mouse_monitor() -> (
    MouseMonitorStarter,
    std::sync::mpsc::Receiver<MouseMonitorEvent>,
) {
    let (events, receiver) = std::sync::mpsc::channel();
    (
        MouseMonitorStarter {
            state: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            desired: Arc::new(AtomicBool::new(false)),
            events,
        },
        receiver,
    )
}

#[cfg(target_os = "windows")]
fn mouse_monitor() -> (
    MouseMonitorStarter,
    std::sync::mpsc::Receiver<MouseMonitorEvent>,
) {
    use std::sync::atomic::{AtomicU8, AtomicU32};

    let (events, receiver) = std::sync::mpsc::channel();
    (
        MouseMonitorStarter {
            state: Arc::new(AtomicU8::new(0)),
            desired: Arc::new(AtomicBool::new(false)),
            thread_id: Arc::new(AtomicU32::new(0)),
            events,
        },
        receiver,
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn mouse_monitor() -> (
    MouseMonitorStarter,
    std::sync::mpsc::Receiver<MouseMonitorEvent>,
) {
    let (_events, receiver) = std::sync::mpsc::channel();
    (MouseMonitorStarter, receiver)
}

#[cfg(target_os = "windows")]
struct WindowsMouseHookCallback {
    desired: Arc<AtomicBool>,
    events: std::sync::mpsc::Sender<MouseMonitorEvent>,
    started: Instant,
}

#[cfg(target_os = "windows")]
fn windows_mouse_hook_callback() -> &'static Mutex<Option<WindowsMouseHookCallback>> {
    static CALLBACK: std::sync::OnceLock<Mutex<Option<WindowsMouseHookCallback>>> =
        std::sync::OnceLock::new();
    CALLBACK.get_or_init(|| Mutex::new(None))
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn windows_mouse_hook_proc(
    code: i32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HC_ACTION, MSLLHOOKSTRUCT};

    if code == HC_ACTION as i32 && lparam != 0 {
        let mouse_data = unsafe { (*(lparam as *const MSLLHOOKSTRUCT)).mouseData };
        if let Some(input) = windows_mouse_message_to_input(wparam as u32, mouse_data, 0) {
            if let Ok(callback) = windows_mouse_hook_callback().try_lock() {
                if let Some(callback) = callback
                    .as_ref()
                    .filter(|state| state.desired.load(Ordering::Acquire))
                {
                    let _ = callback
                        .events
                        .send(MouseMonitorEvent::Input(MouseInputEvent {
                            timestamp_ms: callback.started.elapsed().as_millis() as u64,
                            ..input
                        }));
                }
            }
        }
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

#[cfg(target_os = "windows")]
fn windows_mouse_message_to_input(
    message: u32,
    mouse_data: u32,
    timestamp_ms: u64,
) -> Option<MouseInputEvent> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
        WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
    };

    let (button, phase) = match message {
        WM_LBUTTONDOWN => (0, MousePhase::Down),
        WM_LBUTTONUP => (0, MousePhase::Up),
        WM_RBUTTONDOWN => (1, MousePhase::Down),
        WM_RBUTTONUP => (1, MousePhase::Up),
        WM_MBUTTONDOWN => (2, MousePhase::Down),
        WM_MBUTTONUP => (2, MousePhase::Up),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let button = match (mouse_data >> 16) as u16 {
                XBUTTON1 => 3,
                XBUTTON2 => 4,
                _ => return None,
            };
            let phase = if message == WM_XBUTTONDOWN {
                MousePhase::Down
            } else {
                MousePhase::Up
            };
            (button, phase)
        }
        _ => return None,
    };
    Some(MouseInputEvent {
        button,
        phase,
        timestamp_ms,
    })
}

#[cfg(target_os = "macos")]
fn mac_mouse_event_access(request: bool) -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }

    unsafe { CGPreflightListenEventAccess() || (request && CGRequestListenEventAccess()) }
}

#[derive(Clone, serde::Serialize)]
struct Config {
    server: String,
    token: String,
    device: String,
    interval: u64,
    display: u32,
    roi: Roi,
    auto_capture: bool,
    quick_capture_enabled: bool,
    keyboard_trigger_enabled: bool,
    mouse_trigger_enabled: bool,
    mouse_button: Option<u32>,
    mouse_trigger_action: MouseTriggerAction,
}

fn websocket_connection_settings_changed(current: &Config, next: &Config) -> bool {
    current.server != next.server || current.token != next.token || current.device != next.device
}

fn apply_runtime_config(
    current: &Mutex<Config>,
    next: Config,
    reconnect: &WebsocketReconnectSignal,
) -> bool {
    let mut current = current.lock().unwrap();
    let connection_changed = websocket_connection_settings_changed(&current, &next);
    *current = next;
    drop(current);
    if connection_changed {
        reconnect.notify();
    }
    connection_changed
}

#[derive(Default)]
struct WebsocketReconnectSignal {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl WebsocketReconnectSignal {
    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    fn changed_since(&self, generation: u64) -> bool {
        self.generation() != generation
    }

    fn notify(&self) {
        let mut generation = self.generation.lock().unwrap();
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn wait_for_change(&self, generation: u64, timeout: Duration) -> bool {
        let current = self.generation.lock().unwrap();
        if *current != generation {
            return true;
        }
        let (current, _) = self
            .changed
            .wait_timeout_while(current, timeout, |current| *current == generation)
            .unwrap();
        *current != generation
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            server: "https://ot.sunyibud.online".into(),
            token: "local-dev-token".into(),
            device: default_device_id().into(),
            interval: 5,
            display: 1,
            roi: Roi::full(),
            auto_capture: false,
            quick_capture_enabled: true,
            keyboard_trigger_enabled: true,
            mouse_trigger_enabled: false,
            mouse_button: None,
            mouse_trigger_action: MouseTriggerAction::Single,
        }
    }
}
fn config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        return windows_config_path(std::env::var_os("LOCALAPPDATA").map(PathBuf::from));
    }
    #[cfg(not(target_os = "windows"))]
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/SightRelay/config.env")
}
fn load_config() -> Config {
    let mut c = fs::read_to_string(config_path())
        .map(|text| parse_config_text(&text))
        .unwrap_or_default();
    if c.token.is_empty() || c.token == "replace-with-a-long-random-token" {
        c.token = "local-dev-token".into();
    }
    if let Ok(token) = std::env::var("SIGHT_DEVICE_TOKEN")
        && !token.is_empty()
    {
        c.token = token;
    }
    // Ignore legacy AUTO_CAPTURE=true values from older installations.
    enforce_manual_capture(&mut c);
    c
}

fn parse_config_text(text: &str) -> Config {
    let mut c = Config::default();
    for line in text.lines() {
        let mut parts = line.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        match key {
            "SERVER_URL" => c.server = value.into(),
            "DEVICE_TOKEN" => c.token = value.into(),
            "DEVICE_ID" => c.device = value.into(),
            "INTERVAL_SECONDS" => c.interval = value.parse().unwrap_or(5),
            "DISPLAY_ID" => c.display = value.parse().unwrap_or(1),
            "AUTO_CAPTURE" => c.auto_capture = value == "true",
            "ROI_X" => c.roi.x = value.parse().unwrap_or(0.0),
            "ROI_Y" => c.roi.y = value.parse().unwrap_or(0.0),
            "ROI_W" => c.roi.width = value.parse().unwrap_or(1.0),
            "ROI_H" => c.roi.height = value.parse().unwrap_or(1.0),
            "QUICK_CAPTURE_ENABLED" => c.quick_capture_enabled = value == "true",
            "KEYBOARD_TRIGGER_ENABLED" => c.keyboard_trigger_enabled = value == "true",
            "MOUSE_TRIGGER_ENABLED" => c.mouse_trigger_enabled = value == "true",
            "MOUSE_BUTTON" => c.mouse_button = value.parse().ok(),
            "MOUSE_TRIGGER_ACTION" => c.mouse_trigger_action = MouseTriggerAction::parse(value),
            _ => {}
        }
    }
    enforce_manual_capture(&mut c);
    c
}

fn serialize_config(c: &Config) -> String {
    format!(
        "SERVER_URL={}\nDEVICE_TOKEN={}\nDEVICE_ID={}\nINTERVAL_SECONDS={}\nDISPLAY_ID={}\nAUTO_CAPTURE={}\nROI_X={}\nROI_Y={}\nROI_W={}\nROI_H={}\nQUICK_CAPTURE_ENABLED={}\nKEYBOARD_TRIGGER_ENABLED={}\nMOUSE_TRIGGER_ENABLED={}\nMOUSE_BUTTON={}\nMOUSE_TRIGGER_ACTION={}\n",
        c.server,
        c.token,
        c.device,
        c.interval,
        c.display,
        c.auto_capture,
        c.roi.x,
        c.roi.y,
        c.roi.width,
        c.roi.height,
        c.quick_capture_enabled,
        c.keyboard_trigger_enabled,
        c.mouse_trigger_enabled,
        c.mouse_button
            .map(|value| value.to_string())
            .unwrap_or_default(),
        c.mouse_trigger_action.as_env(),
    )
}
fn save_config(c: &Config) -> std::io::Result<()> {
    let mut c = c.clone();
    enforce_manual_capture(&mut c);
    let p = config_path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(p, serialize_config(&c))
}
const UPLOAD_ATTEMPTS: usize = 3;
const UPLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const UPLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const UPLOAD_RETRY_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UploadAction {
    Single,
    MultiAppend,
    MultiSubmit,
    MultiCancel,
}

struct UploadRequest {
    config: Config,
    action: UploadAction,
}

type UploadSender = mpsc::Sender<UploadRequest>;
static UPLOAD_SENDER: OnceLock<UploadSender> = OnceLock::new();

fn upload_sender() -> &'static UploadSender {
    UPLOAD_SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<UploadRequest>();
        thread::Builder::new()
            .name("sight-relay-upload".into())
            .spawn(move || upload_worker(receiver))
            .expect("start upload worker");
        sender
    })
}

fn upload(c: &Config, gate: &Mutex<bool>) {
    upload_action(c, gate, UploadAction::Single);
}

fn upload_action(c: &Config, gate: &Mutex<bool>, action: UploadAction) {
    if *gate.lock().unwrap() {
        return;
    }
    if upload_sender()
        .send(UploadRequest {
            config: c.clone(),
            action,
        })
        .is_err()
    {
        eprintln!("capture upload worker stopped");
    }
}

fn upload_worker(receiver: mpsc::Receiver<UploadRequest>) {
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(UPLOAD_CONNECT_TIMEOUT)
        .timeout(UPLOAD_REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("failed to create capture upload client: {error}");
            return;
        }
    };
    while let Ok(request) = receiver.recv() {
        let result = match request.action {
            UploadAction::Single | UploadAction::MultiAppend => {
                let image = match capture_display(request.config.display, request.config.roi) {
                    Ok(image) => image,
                    Err(error) => {
                        eprintln!("screen capture failed: {error}");
                        continue;
                    }
                };
                let action = match request.action {
                    UploadAction::Single => "single",
                    UploadAction::MultiAppend => "multi_append",
                    _ => unreachable!(),
                };
                retry_upload(|| send_capture(&client, &request.config, &image.bytes, action))
            }
            UploadAction::MultiSubmit => {
                retry_upload(|| send_group_action(&client, &request.config, "submit"))
            }
            UploadAction::MultiCancel => {
                retry_upload(|| send_group_action(&client, &request.config, "cancel"))
            }
        };
        if let Err(error) = result {
            eprintln!("capture upload failed after {UPLOAD_ATTEMPTS} attempts: {error}");
        }
    }
}

fn send_capture(
    client: &reqwest::blocking::Client,
    config: &Config,
    bytes: &[u8],
    action: &str,
) -> Result<(), String> {
    let part = reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
        .file_name("capture.jpg")
        .mime_str("image/jpeg")
        .map_err(|error| error.to_string())?;
    let form = reqwest::blocking::multipart::Form::new()
        .text("token", config.token.clone())
        .text("device_id", config.device.clone())
        .text("capture_action", action.to_string())
        .part("image", part);
    let response = client
        .post(format!(
            "{}/api/v1/captures",
            config.server.trim_end_matches('/')
        ))
        .multipart(form)
        .send()
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        eprintln!("capture uploaded successfully: {}", response.status());
        Ok(())
    } else {
        Err(format!("server returned {}", response.status()))
    }
}

fn send_group_action(
    client: &reqwest::blocking::Client,
    config: &Config,
    action: &str,
) -> Result<(), String> {
    let form = reqwest::blocking::multipart::Form::new()
        .text("token", config.token.clone())
        .text("device_id", config.device.clone());
    let response = client
        .post(format!(
            "{}/api/v1/capture-groups/{action}",
            config.server.trim_end_matches('/')
        ))
        .multipart(form)
        .send()
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("server returned {}", response.status()))
    }
}

fn retry_upload<F>(mut operation: F) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    let mut last_error = String::from("upload failed");
    for attempt in 0..UPLOAD_ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = error;
                if attempt + 1 < UPLOAD_ATTEMPTS {
                    thread::sleep(UPLOAD_RETRY_DELAY);
                }
            }
        }
    }
    Err(last_error)
}

#[cfg(test)]
fn spawn_upload_worker_for_test<T, F>(
    receiver: mpsc::Receiver<T>,
    operation: F,
) -> thread::JoinHandle<()>
where
    T: Send + 'static,
    F: Fn(T) + Send + 'static,
{
    thread::spawn(move || {
        while let Ok(item) = receiver.recv() {
            operation(item);
        }
    })
}
fn start_websocket(
    config: Arc<Mutex<Config>>,
    connected: Arc<AtomicBool>,
    last: Arc<Mutex<String>>,
    reconnects: Arc<std::sync::atomic::AtomicU64>,
    reconnect_signal: Arc<WebsocketReconnectSignal>,
) {
    thread::spawn(move || {
        'reconnect: loop {
            let generation = reconnect_signal.generation();
            let c = config.lock().unwrap().clone();
            if reconnect_signal.changed_since(generation) {
                continue;
            }
            let url = c
                .server
                .replace("https://", "wss://")
                .replace("http://", "ws://")
                + "/api/v1/ws";
            let result = url
                .parse::<tungstenite::http::Uri>()
                .ok()
                .and_then(|uri| tungstenite::connect(uri).ok());
            if let Some((mut socket, _)) = result {
                connected.store(false, Ordering::Relaxed);
                *last.lock().unwrap() = "Socket 已连接，等待心跳确认".into();
                loop {
                    if reconnect_signal.changed_since(generation) {
                        connected.store(false, Ordering::Relaxed);
                        *last.lock().unwrap() = "配置已更新，正在重新连接".into();
                        let _ = socket.close(None);
                        continue 'reconnect;
                    }
                    let msg = serde_json::json!({"type":"heartbeat","device_id":c.device,"token":c.token}).to_string();
                    if socket.send(tungstenite::Message::Text(msg.into())).is_err() {
                        break;
                    }
                    match socket.read() {
                        Ok(tungstenite::Message::Text(v)) if v.contains("heartbeat_ack") => {
                            connected.store(true, Ordering::Relaxed);
                            *last.lock().unwrap() =
                                format!("心跳已收到 {:?}", std::time::SystemTime::now());
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    if reconnect_signal.wait_for_change(generation, Duration::from_secs(10)) {
                        connected.store(false, Ordering::Relaxed);
                        *last.lock().unwrap() = "配置已更新，正在重新连接".into();
                        let _ = socket.close(None);
                        continue 'reconnect;
                    }
                }
            }
            connected.store(false, Ordering::Relaxed);
            reconnects.fetch_add(1, Ordering::Relaxed);
            *last.lock().unwrap() = "断线，重连中".into();
            thread::sleep(Duration::from_secs(3));
        }
    });
}

#[allow(unreachable_code)]
fn main() {
    let _ = dotenvy::dotenv();
    let mut initial_config = load_config();
    initial_config.display =
        selected_display(initial_config.display, &list_displays().unwrap_or_default());
    enforce_manual_capture(&mut initial_config);
    let active = Arc::new(AtomicBool::new(false));
    let config = Arc::new(std::sync::Mutex::new(initial_config));
    let capture_gate = Arc::new(Mutex::new(false));
    let ws_connected = Arc::new(AtomicBool::new(false));
    let ws_last = Arc::new(Mutex::new("未连接".to_string()));
    let ws_reconnects = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let websocket_reconnect = Arc::new(WebsocketReconnectSignal::default());
    start_websocket(
        config.clone(),
        ws_connected.clone(),
        ws_last.clone(),
        ws_reconnects.clone(),
        websocket_reconnect.clone(),
    );
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        web_ui::run(
            config.clone(),
            active.clone(),
            capture_gate.clone(),
            ws_connected.clone(),
            websocket_reconnect,
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let manager = GlobalHotKeyManager::new().ok();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let hotkey = HotKey::new(Some(Modifiers::ALT), Code::KeyA);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if let Some(m) = &manager {
        let _ = m.register(hotkey);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1448.0, 1086.0])
            .with_min_inner_size([1100.0, 820.0]),
        ..Default::default()
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = eframe::run_native(
        "Sight Relay Capture",
        opts,
        Box::new(move |cc| {
            configure_fonts(&cc.egui_ctx);
            Ok(Box::new(App {
                config,
                active,
                _manager: manager,
                capture_gate,
                editor: None,
                hotkey,
                status: "未连接".into(),
                ws_connected,
                ws_last,
                ws_reconnects,
            }))
        }),
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if let Err(e) = result {
        eprintln!("UI: {e}");
    }
}
struct App {
    config: Arc<std::sync::Mutex<Config>>,
    active: Arc<AtomicBool>,
    _manager: Option<GlobalHotKeyManager>,
    capture_gate: Arc<Mutex<bool>>,
    editor: Option<mpsc::Receiver<Result<Option<Roi>, String>>>,
    hotkey: HotKey,
    status: String,
    ws_connected: Arc<AtomicBool>,
    ws_last: Arc<Mutex<String>>,
    ws_reconnects: Arc<std::sync::atomic::AtomicU64>,
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_secs(1));
        if let Some(receiver) = &self.editor {
            match receiver.try_recv() {
                Ok(result) => {
                    self.editor = None;
                    // The native window has already closed and the compositor has settled.
                    if let Ok(Some(roi)) = result.as_ref() {
                        let mut config = self.config.lock().unwrap();
                        let mut candidate = config.clone();
                        candidate.roi = *roi;
                        match save_config(&candidate) {
                            Ok(()) => {
                                *config = candidate;
                                self.status = "捕获范围已保存".into();
                            }
                            Err(e) => self.status = format!("范围保存失败：{e}"),
                        }
                    } else {
                        self.status = match result {
                            Ok(None) => "已取消，保留原捕获范围".into(),
                            Err(e) => e,
                            _ => unreachable!(),
                        };
                    }
                    *self.capture_gate.lock().unwrap() = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.editor = None;
                    *self.capture_gate.lock().unwrap() = false;
                    self.status = "范围编辑器意外退出，保留原范围".into();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
            // Discard screenshot hotkeys pressed while the editor owned the screen.
            while GlobalHotKeyEvent::receiver().try_recv().is_ok() {}
        }
        if let Ok(e) = GlobalHotKeyEvent::receiver().try_recv()
            && e.id == self.hotkey.id()
            && e.state == HotKeyState::Pressed
        {
            let c = self.config.lock().unwrap().clone();
            upload(&c, &self.capture_gate);
            self.status = "快捷键截图已上传".into();
        }
        let mut c = self.config.lock().unwrap().clone();
        ctx.set_visuals(egui::Visuals::dark());
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(9, 17, 29))
                    .inner_margin(egui::Margin::symmetric(46, 0)),
            )
            .show(ctx, |ui| {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
                        ui.set_max_width(1356.0);
                        ui.add_space(18.0);

                        let online = self.ws_connected.load(Ordering::Relaxed);
                        let heartbeat = self.ws_last.lock().unwrap().clone();
                        let reconnects = self.ws_reconnects.load(Ordering::Relaxed);
                        let running = false;

                        // Header / brand row.
                        ui.horizontal(|ui| {
                            egui::Frame::NONE
                                .fill(egui::Color32::from_rgb(53, 103, 241))
                                .corner_radius(egui::CornerRadius::same(15))
                                .inner_margin(egui::Margin::same(16))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("⌗")
                                            .size(42.0)
                                            .strong()
                                            .color(egui::Color32::WHITE),
                                    );
                                });
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Sight Relay")
                                        .size(38.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(245, 248, 255)),
                                );
                                ui.label(
                                    egui::RichText::new("屏幕采集控制台")
                                        .size(18.0)
                                        .color(egui::Color32::from_rgb(147, 164, 190)),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let status_fill = if online {
                                        egui::Color32::from_rgb(18, 82, 77)
                                    } else {
                                        egui::Color32::from_rgb(63, 28, 48)
                                    };
                                    let status_text = if online { "已连接" } else { "未连接" };
                                    pill(ui, status_text, status_fill, if online {
                                        egui::Color32::from_rgb(79, 224, 177)
                                    } else {
                                        egui::Color32::from_rgb(255, 111, 125)
                                    });
                                    pill(ui, if running { "▶  采集中" } else { "Ⅱ  暂停中" }, egui::Color32::from_rgb(24, 37, 55), egui::Color32::from_rgb(221, 230, 245));
                                    pill(ui, &format!("⌁  心跳  {}", if online { "正常" } else if heartbeat.contains("重连") { "断线" } else { "等待" }), egui::Color32::from_rgb(24, 37, 55), egui::Color32::from_rgb(221, 230, 245));
                                    pill(ui, &format!("⟳  重连  {}", reconnects), egui::Color32::from_rgb(24, 37, 55), egui::Color32::from_rgb(221, 230, 245));
                                },
                            );
                        });
                        ui.add_space(12.0);

                        // Status summary card.
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgb(19, 34, 56))
                            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(47, 78, 113)))
                            .corner_radius(egui::CornerRadius::same(14))
                            .inner_margin(egui::Margin::symmetric(28, 22))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    egui::Frame::NONE
                                        .fill(egui::Color32::from_rgb(176, 126, 24))
                                        .corner_radius(egui::CornerRadius::same(40))
                                        .inner_margin(egui::Margin::same(20))
                                        .show(ui, |ui| {
                                            ui.label(egui::RichText::new("Ⅱ").size(27.0).strong().color(egui::Color32::from_rgb(28, 35, 45)));
                                        });
                                    ui.add_space(16.0);
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new("当前状态").size(16.0).color(egui::Color32::from_rgb(157, 178, 209)));
                                        ui.label(egui::RichText::new(if running { "采集中" } else { "已暂停" }).size(34.0).strong().color(egui::Color32::WHITE));
                                        ui.label(egui::RichText::new("管理员已关闭自动采集功能，建议通过快捷键手动采集").size(16.0).color(egui::Color32::from_rgb(157, 178, 209)));
                                    });
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.separator();
                                        ui.add_space(18.0);
                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.colored_label(egui::Color32::from_rgb(255, 99, 111), "●");
                                                ui.label(egui::RichText::new(if online { "已连接" } else { "未连接" }).size(17.0).strong().color(egui::Color32::WHITE));
                                            });
                                            ui.label(egui::RichText::new(if online { "服务端连接正常" } else { "等待连接到服务器" }).size(14.0).color(egui::Color32::from_rgb(157, 178, 209)));
                                        });
                                    });
                                });
                            });
                        ui.add_space(16.0);

                        // Connection and capture settings.
                        ui.columns(2, |cols| {
                            let (left, right) = cols.split_at_mut(1);
                            section_card(&mut left[0], "⌁", "连接设置", "配置服务器地址与设备认证信息", |ui| {
                                field_label(ui, "Server 地址");
                                text_field(ui, &mut c.server, "https://ot.sunyibud.online", false);
                                ui.add_space(11.0);
                                field_label(ui, "设备 Token");
                                text_field(ui, &mut c.token, "请输入设备 Token", true);
                            });
                            section_card(&mut right[0], "⚙", "采集设置", "设置设备标识与采集参数", |ui| {
                                field_label(ui, "设备名称");
                                text_field(ui, &mut c.device, "mac-m5", false);
                                ui.add_space(11.0);
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        field_label(ui, "显示器");
                                        let mut display = c.display.to_string();
                                        if ui.add_sized([130.0, 38.0], egui::TextEdit::singleline(&mut display)).changed() { c.display = display.parse().unwrap_or(1); }
                                    });
                                    ui.add_space(14.0);
                                    ui.vertical(|ui| {
                                        field_label(ui, "间隔（秒）");
                                        let mut interval = c.interval.to_string();
                                        ui.add_enabled(false, egui::TextEdit::singleline(&mut interval).desired_width(100.0));
                                    });
                                    ui.add_space(16.0);
                                    ui.vertical(|ui| {
                                        field_label(ui, "自动采集");
                                        let mut enabled = false;
                                        ui.add_enabled(false, egui::Checkbox::new(&mut enabled, "已关闭"));
                                    });
                                });
                            });
                        });
                        ui.add_space(10.0);

                        section_card(ui, "⌗", "截图范围", "在实际屏幕上拖动四角或边缘缩放，拖动框内移动。", |ui| {
                            ui.horizontal(|ui| {
                                egui::Frame::NONE
                                    .fill(egui::Color32::from_rgb(16, 31, 51))
                                    .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(48, 81, 120)))
                                    .corner_radius(egui::CornerRadius::same(12))
                                    .inner_margin(egui::Margin::symmetric(16, 12))
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new("ⓘ").size(18.0).color(egui::Color32::from_rgb(77, 156, 255)));
                                        ui.vertical(|ui| {
                                            ui.label(egui::RichText::new("按住 Enter 保存  ·  Esc 取消  ·  编辑期间暂停采集").size(14.0).color(egui::Color32::from_rgb(195, 210, 233)));
                                            ui.label(egui::RichText::new(format!("当前范围：左 {:.1}%  ·  上 {:.1}%  ·  宽 {:.1}%  ·  高 {:.1}%", c.roi.x * 100.0, c.roi.y * 100.0, c.roi.width * 100.0, c.roi.height * 100.0)).size(14.0).color(egui::Color32::from_rgb(145, 166, 197)));
                                        });
                                    });
                                ui.add_space(16.0);
                                if outline_button(ui, "⌾  设置捕获范围").clicked() { self.start_roi_editor(ctx, &c); }
                            });
                        });
                        ui.add_space(10.0);

                        ui.columns(4, |cols| {
                            if action_button(&mut cols[0], "⌁  测试连接", egui::Color32::from_rgb(29, 50, 78)).clicked() {
                                let url = format!("{}/healthz", c.server.trim_end_matches('/'));
                                self.status = match reqwest::blocking::get(url) { Ok(r) if r.status().is_success() => "连接正常".into(), _ => "连接失败".into() };
                            }
                            if action_button(&mut cols[1], "▧  测试截图", egui::Color32::from_rgb(29, 50, 78)).clicked() { upload(&c, &self.capture_gate); self.status = "测试截图已上传".into(); }
                            if action_button(&mut cols[2], "⛔  自动采集已关闭", egui::Color32::from_rgb(58, 68, 87)).clicked() {
                                self.status = automatic_capture_disabled_message().into();
                            }
                            if action_button(&mut cols[3], "▣  保存配置", egui::Color32::from_rgb(36, 53, 84)).clicked() {
                                c.token = c.token.trim().to_string();
                                c.device = c.device.trim().to_string();
                                self.status = match save_config(&c) {
                                    Ok(()) => {
                                        *self.config.lock().unwrap() = c.clone();
                                        self.ws_connected.store(false, Ordering::Relaxed);
                                        "配置已保存，连接将在下一次重试时使用新 Token".into()
                                    }
                                    Err(e) => format!("配置保存失败：{e}"),
                                };
                            }
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("快捷键").size(13.0).color(egui::Color32::from_rgb(151, 170, 198)));
                            for key in ["⌥", "A"] { ui.add(egui::Label::new(egui::RichText::new(key).monospace().size(13.0).color(egui::Color32::from_rgb(220, 230, 248))).sense(egui::Sense::hover())); }
                            ui.label(egui::RichText::new("立即截图").size(13.0).color(egui::Color32::from_rgb(151, 170, 198)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { ui.label(egui::RichText::new("Sight Relay  ·  让屏幕数据触手可及").size(13.0).color(egui::Color32::from_rgb(126, 144, 170))); });
                        });
                        ui.add_space(8.0);
                });
            });
        *self.config.lock().unwrap() = c;
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
    ];
    if let Some(path) = candidates.iter().find(|p| std::path::Path::new(p).exists())
        && let Ok(bytes) = std::fs::read(path)
    {
        fonts.font_data.insert(
            "mac_chinese".into(),
            egui::FontData::from_owned(bytes).into(),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "mac_chinese".into());
        }
    }
    ctx.set_fonts(fonts);
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(13.0)
            .color(egui::Color32::from_rgb(186, 204, 231)),
    );
}

fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str, password: bool) {
    let edit = egui::TextEdit::singleline(value)
        .hint_text(hint)
        .desired_width(f32::INFINITY)
        .password(password)
        .margin(egui::Margin::symmetric(12, 9));
    ui.add(edit);
}

fn section_card<F>(ui: &mut egui::Ui, glyph: &str, title: &str, subtitle: &str, add_contents: F)
where
    F: FnOnce(&mut egui::Ui),
{
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(24, 40, 63))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgb(61, 96, 139),
        ))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(23, 20))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(39, 83, 144))
                    .corner_radius(egui::CornerRadius::same(30))
                    .inner_margin(egui::Margin::same(9))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(glyph)
                                .size(24.0)
                                .color(egui::Color32::from_rgb(222, 236, 255)),
                        );
                    });
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .size(23.0)
                            .strong()
                            .color(egui::Color32::from_rgb(245, 248, 255)),
                    );
                    ui.label(
                        egui::RichText::new(subtitle)
                            .size(15.0)
                            .color(egui::Color32::from_rgb(142, 164, 195)),
                    );
                });
            });
            ui.add_space(14.0);
            add_contents(ui);
        });
}

fn pill(ui: &mut egui::Ui, text: &str, fill: egui::Color32, text_color: egui::Color32) {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(13.0).color(text_color))
            .fill(fill)
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(54, 76, 105),
            ))
            .corner_radius(egui::CornerRadius::same(20))
            .min_size(egui::vec2(146.0, 54.0)),
    );
}

fn outline_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized(
        [273.0, 96.0],
        egui::Button::new(
            egui::RichText::new(text)
                .size(18.0)
                .strong()
                .color(egui::Color32::from_rgb(229, 239, 255)),
        )
        .fill(egui::Color32::from_rgb(41, 79, 133))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgb(85, 143, 221),
        ))
        .corner_radius(egui::CornerRadius::same(14)),
    )
}

fn action_button(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let button = egui::Button::new(
        egui::RichText::new(text)
            .size(18.0)
            .strong()
            .color(egui::Color32::from_rgb(231, 239, 252)),
    )
    .fill(color)
    .stroke(egui::Stroke::new(
        1.0_f32,
        egui::Color32::from_rgb(73, 103, 143),
    ))
    .corner_radius(egui::CornerRadius::same(12));
    ui.add_sized([ui.available_width(), 70.0], button)
}

impl App {
    fn start_roi_editor(&mut self, ctx: &egui::Context, c: &Config) {
        let helper = std::env::current_exe()
            .ok()
            .map(|path| roi_helper_path(&path));
        let Some(helper) = helper.filter(|p| p.is_file()) else {
            self.status =
                "未找到范围编辑器。请使用最新 App，或先运行 deploy/build-roi.sh debug".into();
            return;
        };
        // Serialize with the actual screen read (not the HTTP upload), so no capture
        // can start between pausing and displaying the overlay.
        *self.capture_gate.lock().unwrap() = true;
        let (tx, rx) = mpsc::channel();
        self.editor = Some(rx);
        let c = c.clone();
        let preview_path =
            std::env::temp_dir().join(format!("sight-relay-preview-{}.jpg", std::process::id()));
        let preview_path_arg = preview_path.to_string_lossy().to_string();
        match capture_display(c.display, Roi::full()).and_then(|image| {
            fs::write(&preview_path, image.bytes)
                .map_err(|e| mac_capture::CaptureError::Screen(e.to_string()))
        }) {
            Ok(()) => {}
            Err(e) => {
                *self.capture_gate.lock().unwrap() = false;
                self.editor = None;
                self.status = format!("无法生成当前画面预览：{e}");
                return;
            }
        }
        let repaint = ctx.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        thread::spawn(move || {
            // Let the settings window disappear before the native overlay appears.
            thread::sleep(Duration::from_millis(250));
            let result = std::process::Command::new(helper)
                .args([
                    c.display.to_string(),
                    preview_path_arg.clone(),
                    c.roi.x.to_string(),
                    c.roi.y.to_string(),
                    c.roi.width.to_string(),
                    c.roi.height.to_string(),
                ])
                .output()
                .map_err(|e| format!("无法打开范围编辑器：{e}"))
                .and_then(|output| {
                    if !output.status.success() {
                        return Err(format!(
                            "范围编辑失败：{}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                    parse_roi_result(&output.stdout)
                });
            thread::sleep(Duration::from_millis(250));
            let _ = fs::remove_file(&preview_path);
            let _ = tx.send(result);
            repaint.request_repaint();
        });
    }
}

fn parse_roi_result(bytes: &[u8]) -> Result<Option<Roi>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let roi: Roi =
        serde_json::from_slice(bytes).map_err(|_| "范围编辑器返回无效数据".to_string())?;
    if [roi.x, roi.y, roi.width, roi.height]
        .iter()
        .any(|v| !v.is_finite())
        || roi.x < 0.0
        || roi.y < 0.0
        || roi.width <= 0.0
        || roi.height <= 0.0
        || roi.x + roi.width > 1.00001
        || roi.y + roi.height > 1.00001
    {
        return Err("范围编辑器返回越界范围".into());
    }
    Ok(Some(roi))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod roi_editor_tests {
    use super::{
        Config, MouseInputEvent, MousePhase, MouseTriggerAction, MouseTriggerRecognizer,
        automatic_capture_disabled_message, begin_mouse_upload, default_mouse_action,
        enforce_manual_capture, parse_config_text, parse_roi_result, serialize_config,
    };
    use std::path::{Path, PathBuf};

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_client_uses_local_app_data_and_windows_shortcut_labels() {
        let path =
            super::windows_config_path(Some(PathBuf::from(r"C:\\Users\\Ada\\AppData\\Local")));
        assert_eq!(
            path,
            PathBuf::from(r"C:\\Users\\Ada\\AppData\\Local\\SightRelay\\config.env")
        );
        assert_eq!(super::platform_presentation().capture_key, "Alt");
        assert_eq!(super::platform_presentation().quit_shortcut, "Ctrl+Q");
        assert_eq!(Config::default().device, "windows-pc");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_mouse_monitor_failure_does_not_request_macos_permissions() {
        assert_eq!(
            super::mouse_monitor_unavailable_message(),
            "无法启动鼠标监听，请关闭冲突的鼠标工具后重试"
        );
    }

    #[test]
    fn selected_display_prefers_saved_id_and_falls_back_to_first_available_display() {
        let displays = vec![
            (812_u32, "Built-in display".to_string(), 1920, 1080),
            (913_u32, "External display".to_string(), 2560, 1440),
        ];
        assert_eq!(super::selected_display(913, &displays), 913);
        assert_eq!(super::selected_display(1, &displays), 812);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_roi_helper_is_packaged_beside_the_client() {
        assert_eq!(
            super::roi_helper_path(Path::new(r"C:\\Program Files\\SightRelay\\SightRelay.exe")),
            PathBuf::from(r"C:\\Program Files\\SightRelay\\roi-overlay.exe")
        );
    }

    #[test]
    fn shared_capture_page_accepts_platform_shortcuts_and_real_display_data() {
        let page = include_str!("../../../../web/capture-client.html");
        assert!(page.contains("window.__DISPLAYS__"));
        assert!(page.contains("window.__PLATFORM__"));
        assert!(page.contains("captureShortcutHint"));
        assert!(page.contains("captureShortcutKeys"));
        assert!(page.contains("quickCaptureSwitch"));
        assert!(page.contains("keyboardTriggerSwitch"));
        assert!(page.contains("mouseTriggerSwitch"));
        assert!(page.contains("start_mouse_binding"));
        assert!(page.contains("test_mouse_trigger"));
        assert!(page.contains("clear_mouse_binding"));
        assert!(page.contains("nativeCommand('test_mouse_trigger',{mouse_button:mouseButton,mouse_trigger_action:mouseTriggerAction})"));
        assert!(!page.contains("persistCurrentConfig();startBindingCountdown('test')"));
        assert!(!page.contains("renderAuto()"));
        assert_eq!(
            super::platform_presentation().mouse_trigger_supported,
            cfg!(any(target_os = "macos", target_os = "windows"))
        );
    }

    #[test]
    fn capture_footer_describes_single_and_group_shortcuts() {
        let page = include_str!("../../../../web/capture-client.html");
        for (key, action) in [
            ("A", "单题立即解析"),
            ("S", "加入题目组"),
            ("Z", "提交题目组"),
            ("X", "清空题目组"),
        ] {
            assert!(page.contains(&format!("data-shortcut-key=\"{key}\"")));
            assert!(page.contains(action));
        }
    }

    #[test]
    fn release_tags_are_compared_as_semver_without_a_v_prefix() {
        assert!(super::is_newer_release("1.2.0", "v1.3.0"));
        assert!(!super::is_newer_release("1.3.0", "v1.2.0"));
        assert!(!super::is_newer_release("1.3.0", "not-a-version"));
        assert!(super::is_newer_release("1.2.0", "1.2.1-beta.1"));
    }

    #[test]
    fn release_check_uses_the_fixed_gitea_api_and_release_page() {
        assert_eq!(
            super::RELEASE_API_URL,
            "https://git.sunyibud.online/api/v1/repos/sunyibud/sight-relay-release/releases/latest"
        );
        assert_eq!(
            super::RELEASE_PAGE_URL,
            "https://git.sunyibud.online/sunyibud/sight-relay-release/releases"
        );
    }

    #[test]
    fn capture_page_contains_a_non_blocking_update_banner_and_update_action() {
        let page = include_str!("../../../../web/capture-client.html");
        assert!(page.contains("id=\"updateBanner\""));
        assert!(page.contains("id=\"updateOpen\""));
        assert!(page.contains("nativeCommand('open_release_page')"));
        assert!(page.contains("window.__updateCheckResult"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_mouse_messages_map_to_shared_button_events() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN,
            WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
        };

        let cases = [
            (WM_LBUTTONDOWN, 0, 0, MousePhase::Down),
            (WM_LBUTTONUP, 0, 0, MousePhase::Up),
            (WM_RBUTTONDOWN, 0, 1, MousePhase::Down),
            (WM_RBUTTONUP, 0, 1, MousePhase::Up),
            (WM_MBUTTONDOWN, 0, 2, MousePhase::Down),
            (WM_MBUTTONUP, 0, 2, MousePhase::Up),
            (WM_XBUTTONDOWN, 1_u32 << 16, 3, MousePhase::Down),
            (WM_XBUTTONUP, 1_u32 << 16, 3, MousePhase::Up),
            (WM_XBUTTONDOWN, 2_u32 << 16, 4, MousePhase::Down),
            (WM_XBUTTONUP, 2_u32 << 16, 4, MousePhase::Up),
        ];

        for (message, mouse_data, button, phase) in cases {
            assert_eq!(
                super::windows_mouse_message_to_input(message, mouse_data, 123),
                Some(MouseInputEvent {
                    button,
                    phase,
                    timestamp_ms: 123,
                })
            );
        }
        assert_eq!(super::windows_mouse_message_to_input(0, 0, 123), None);
        assert_eq!(
            super::windows_mouse_message_to_input(WM_XBUTTONDOWN, 0, 123),
            None
        );
    }

    #[test]
    fn quick_capture_defaults_preserve_option_a_and_leave_mouse_unbound() {
        let config = Config::default();
        assert!(config.quick_capture_enabled);
        assert!(config.keyboard_trigger_enabled);
        assert!(!config.mouse_trigger_enabled);
        assert_eq!(config.mouse_button, None);
        assert_eq!(config.mouse_trigger_action, MouseTriggerAction::Single);
    }

    #[test]
    fn legacy_config_keeps_keyboard_capture_defaults() {
        let config = parse_config_text("SERVER_URL=http://127.0.0.1:8080\nDEVICE_ID=mac\n");
        assert!(config.quick_capture_enabled);
        assert!(config.keyboard_trigger_enabled);
        assert!(!config.mouse_trigger_enabled);
        assert_eq!(config.mouse_button, None);
    }

    #[test]
    fn mouse_trigger_settings_round_trip_through_env_format() {
        let mut config = Config::default();
        config.quick_capture_enabled = false;
        config.keyboard_trigger_enabled = false;
        config.mouse_trigger_enabled = true;
        config.mouse_button = Some(4);
        config.mouse_trigger_action = MouseTriggerAction::LongPress;

        let loaded = parse_config_text(&serialize_config(&config));
        assert!(!loaded.quick_capture_enabled);
        assert!(!loaded.keyboard_trigger_enabled);
        assert!(loaded.mouse_trigger_enabled);
        assert_eq!(loaded.mouse_button, Some(4));
        assert_eq!(loaded.mouse_trigger_action, MouseTriggerAction::LongPress);
    }

    #[test]
    fn primary_buttons_default_to_double_click_and_side_buttons_to_single_click() {
        assert_eq!(default_mouse_action(0), MouseTriggerAction::Double);
        assert_eq!(default_mouse_action(1), MouseTriggerAction::Double);
        assert_eq!(default_mouse_action(3), MouseTriggerAction::Single);
    }

    #[test]
    fn mouse_trigger_respects_switches_button_action_and_roi_gate() {
        let mut config = Config::default();
        config.mouse_trigger_enabled = true;
        config.mouse_button = Some(3);
        let mut recognizer = MouseTriggerRecognizer::default();

        assert!(recognizer.handle(MouseInputEvent::up(3, 100), &config, false));
        assert!(!recognizer.handle(MouseInputEvent::up(4, 200), &config, false));
        assert!(!recognizer.handle(MouseInputEvent::up(3, 300), &config, true));
        config.quick_capture_enabled = false;
        assert!(!recognizer.handle(MouseInputEvent::up(3, 400), &config, false));
    }

    #[test]
    fn double_click_triggers_only_on_second_release() {
        let mut config = Config::default();
        config.mouse_trigger_enabled = true;
        config.mouse_button = Some(0);
        config.mouse_trigger_action = MouseTriggerAction::Double;
        let mut recognizer = MouseTriggerRecognizer::default();

        assert!(!recognizer.handle(MouseInputEvent::up(0, 100), &config, false));
        assert!(recognizer.handle(MouseInputEvent::up(0, 450), &config, false));
        assert!(!recognizer.handle(MouseInputEvent::up(0, 1200), &config, false));
    }

    #[test]
    fn long_press_triggers_after_threshold_on_release() {
        let mut config = Config::default();
        config.mouse_trigger_enabled = true;
        config.mouse_button = Some(2);
        config.mouse_trigger_action = MouseTriggerAction::LongPress;
        let mut recognizer = MouseTriggerRecognizer::default();

        assert!(!recognizer.handle(
            MouseInputEvent {
                button: 2,
                phase: MousePhase::Down,
                timestamp_ms: 100
            },
            &config,
            false
        ));
        assert!(!recognizer.handle(MouseInputEvent::up(2, 650), &config, false));
        assert!(!recognizer.handle(
            MouseInputEvent {
                button: 2,
                phase: MousePhase::Down,
                timestamp_ms: 1000
            },
            &config,
            false
        ));
        assert!(recognizer.handle(MouseInputEvent::up(2, 1750), &config, false));
    }

    #[test]
    fn mouse_upload_gate_allows_only_one_in_flight_upload() {
        let in_flight = std::sync::atomic::AtomicBool::new(false);
        assert!(begin_mouse_upload(&in_flight));
        assert!(!begin_mouse_upload(&in_flight));
        in_flight.store(false, std::sync::atomic::Ordering::Release);
        assert!(begin_mouse_upload(&in_flight));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_package_builds_and_copies_the_native_roi_editor() {
        let script = include_str!("../../../../deploy/build-windows.ps1");
        assert!(script.contains("--bin roi-overlay"));
        assert!(script.contains("roi-overlay.exe"));
        assert!(!script.contains("Get-Command link.exe"));
        assert!(script.contains("SightRelay-Windows-package"));
        assert!(script.contains("MOUSE_TRIGGER_ENABLED=false"));
        assert!(script.contains("录入鼠标按键"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_release_embeds_the_shared_application_icon() {
        let manifest = include_str!("../../../../crates/mac-capture/Cargo.toml");
        let build_script = include_str!("../../../../crates/mac-capture/build.rs");
        let package_script = include_str!("../../../../deploy/build-windows.ps1");
        assert!(manifest.contains("winresource = \"0.1\""));
        assert!(build_script.contains("assets/sight-relay.ico"));
        assert!(package_script.contains("create-windows-icon.ps1"));
        assert!(package_script.contains("assets\\sight-relay.ico"));
        assert!(package_script.contains("sight-relay.ico"));
        let _ = super::web_ui::application_window_icon();
    }

    #[test]
    fn automatic_capture_is_forced_off_and_explains_manual_shortcut() {
        let mut config = Config::default();
        config.auto_capture = true;
        enforce_manual_capture(&mut config);
        assert!(!config.auto_capture);
        assert_eq!(
            automatic_capture_disabled_message(),
            "管理员已关闭自动采集功能，建议通过快捷键手动采集"
        );
    }

    #[test]
    fn websocket_reconnects_only_when_connection_settings_change() {
        let current = Config::default();

        let mut server = current.clone();
        server.server = "http://127.0.0.1:8080".into();
        assert!(super::websocket_connection_settings_changed(
            &current, &server
        ));

        let mut token = current.clone();
        token.token = "new-device-token".into();
        assert!(super::websocket_connection_settings_changed(
            &current, &token
        ));

        let mut device = current.clone();
        device.device = "another-device".into();
        assert!(super::websocket_connection_settings_changed(
            &current, &device
        ));

        let mut capture_only = current.clone();
        capture_only.display = 2;
        capture_only.roi = super::Roi {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
        };
        assert!(!super::websocket_connection_settings_changed(
            &current,
            &capture_only
        ));
    }

    #[test]
    fn websocket_reconnect_signal_wakes_waiter_after_notification() {
        let signal = super::WebsocketReconnectSignal::default();
        let generation = signal.generation();
        signal.notify();

        assert!(signal.changed_since(generation));
        assert!(signal.wait_for_change(generation, std::time::Duration::from_secs(1)));
    }

    #[test]
    fn applying_new_connection_config_notifies_websocket_worker() {
        let current = std::sync::Mutex::new(Config::default());
        let signal = super::WebsocketReconnectSignal::default();
        let generation = signal.generation();
        let mut next = Config::default();
        next.server = "http://127.0.0.1:8080".into();

        assert!(super::apply_runtime_config(&current, next, &signal));
        assert!(signal.changed_since(generation));
        assert_eq!(current.lock().unwrap().server, "http://127.0.0.1:8080");
    }

    #[test]
    fn cancelled_editor_has_no_roi() {
        assert!(parse_roi_result(b" \n").unwrap().is_none());
    }

    #[test]
    fn confirmed_editor_preserves_normalized_coordinates() {
        let roi = parse_roi_result(br#"{"x":0.25,"y":0.2,"width":0.5,"height":0.6}"#)
            .unwrap()
            .unwrap();
        assert_eq!(roi.x, 0.25);
        assert_eq!(roi.y, 0.2);
        assert_eq!(roi.width, 0.5);
        assert_eq!(roi.height, 0.6);
    }

    #[test]
    fn malformed_or_out_of_screen_results_are_rejected() {
        for bytes in [
            &b"invalid"[..],
            &br#"{"x":0.9,"y":0,"width":0.5,"height":1}"#[..],
            &br#"{"x":0,"y":0,"width":0,"height":1}"#[..],
            &br#"{"x":-0.1,"y":0,"width":1,"height":1}"#[..],
        ] {
            assert!(parse_roi_result(bytes).is_err());
        }
    }
}

#[cfg(test)]
mod upload_tests {
    use super::*;

    #[test]
    fn upload_retry_succeeds_after_two_transient_failures() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let result = retry_upload(|| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                Err("temporary".into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn upload_dispatch_returns_without_waiting_for_worker() {
        let (tx, rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let _worker = spawn_upload_worker_for_test(rx, move |_| {
            started_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        });
        let started = Instant::now();
        tx.send(()).unwrap();
        assert!(started.elapsed() < Duration::from_millis(80));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
